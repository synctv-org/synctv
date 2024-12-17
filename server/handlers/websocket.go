package handlers

import (
	"errors"
	"fmt"
	"io"
	"strings"
	"time"

	"github.com/gin-gonic/gin"
	"github.com/gorilla/websocket"
	log "github.com/sirupsen/logrus"
	dbModel "github.com/synctv-org/synctv/internal/model"
	"github.com/synctv-org/synctv/internal/op"
	pb "github.com/synctv-org/synctv/proto/message"
	"github.com/synctv-org/synctv/utils"
	"google.golang.org/protobuf/proto"
)

const (
	maxInterval          = 10
	MaxChatMessageLength = 4096
)

func NewWebSocketHandler(wss *utils.WebSocket) gin.HandlerFunc {
	return func(ctx *gin.Context) {
		token := ctx.MustGet("token").(string)
		room := ctx.MustGet("room").(*op.RoomEntry).Value()
		user := ctx.MustGet("user").(*op.UserEntry).Value()
		log := ctx.MustGet("log").(*log.Entry)

		subprotocols := []string{}
		if token != "" {
			subprotocols = append(subprotocols, token)
		}

		_ = wss.Server(ctx.Writer, ctx.Request, subprotocols, NewWSMessageHandler(user, room, log))
	}
}

func isNormalCloseError(err error) bool {
	var we *websocket.CloseError
	if !errors.As(err, &we) {
		return false
	}
	return we.Code == websocket.CloseNormalClosure
}

func NewWSMessageHandler(u *op.User, r *op.Room, l *log.Entry) func(c *websocket.Conn) error {
	return func(c *websocket.Conn) error {
		client, err := r.NewClient(u, c)
		if err != nil {
			l.Errorf("ws: register client error: %v", err)
			wc, err2 := c.NextWriter(websocket.BinaryMessage)
			if err2 != nil {
				return err2
			}
			defer wc.Close()
			em := pb.Message{
				Type: pb.MessageType_ERROR,
				Payload: &pb.Message_ErrorMessage{
					ErrorMessage: fmt.Sprintf("register client error: %v", err),
				},
			}
			return em.Encode(wc)
		}

		l.Info("ws: connected")
		defer handleClientDisconnection(r, client, l)

		if err := sendViewerCount(client, r); err != nil {
			l.Errorf("ws: send viewer count error: %v", err)
			return err
		}

		go func() {
			if err := handleReaderMessage(client, l); err != nil {
				if isNormalCloseError(err) {
					return
				}
				l.Errorf("ws: handle reader message error: %v", err)
			}
		}()
		return handleWriterMessage(client, l)
	}
}

func handleClientDisconnection(r *op.Room, client *op.Client, l *log.Entry) {
	if err := r.UnregisterClient(client); err != nil {
		l.Errorf("ws: unregister client error: %v", err)
	}
	client.Close()
	l.Info("ws: disconnected")
}

func sendViewerCount(client *op.Client, r *op.Room) error {
	return client.Send(&pb.Message{
		Type: pb.MessageType_VIEWER_COUNT,
		Payload: &pb.Message_ViewerCount{
			ViewerCount: r.ViewerCount(),
		},
	})
}

func handleWriterMessage(c *op.Client, l *log.Entry) error {
	for v := range c.GetReadChan() {
		if err := writeMessage(c, v); err != nil {
			l.Errorf("ws: write message error: %v", err)
			return err
		}
	}
	return nil
}

func writeMessage(c *op.Client, v op.Message) error {
	wc, err := c.NextWriter(v.MessageType())
	if err != nil {
		return fmt.Errorf("get next writer error: %w", err)
	}
	defer wc.Close()

	if err = v.Encode(wc); err != nil {
		return fmt.Errorf("encode message error: %w", err)
	}

	return nil
}

func handleReaderMessage(c *op.Client, l *log.Entry) error {
	defer func() {
		if c.RTCJoined() {
			c.SetRTCJoined(false)
			_ = c.Broadcast(&pb.Message{
				Type: pb.MessageType_WEBRTC_LEAVE,
				Sender: &pb.Sender{
					Username: c.User().Username,
					UserId:   c.User().ID,
				},
				Payload: &pb.Message_WebrtcData{
					WebrtcData: &pb.WebRTCData{
						From: fmt.Sprintf("%s:%s", c.User().ID, c.ConnID()),
					},
				},
			})
		}
		c.Close()
		if r := recover(); r != nil {
			l.Errorf("ws: panic: %v", r)
		}
	}()

	for {
		msg, err := readMessage(c)
		if err != nil {
			if isNormalCloseError(err) {
				return nil
			}
			l.Errorf("ws: read message error: %v", err)
			return err
		}

		l.Debugf("ws: receive message: %v", msg.String())
		if err = handleElementMsg(c, msg); err != nil {
			l.Errorf("ws: handle message error: %v", err)
			return err
		}
	}
}

func readMessage(c *op.Client) (*pb.Message, error) {
	t, rd, err := c.NextReader()
	if err != nil {
		return nil, fmt.Errorf("get next reader error: %w", err)
	}

	if t != websocket.BinaryMessage {
		return nil, fmt.Errorf("receive unknown message type: %d", t)
	}

	data, err := io.ReadAll(rd)
	if err != nil {
		return nil, fmt.Errorf("read message error: %w", err)
	}

	var msg pb.Message
	if err := proto.Unmarshal(data, &msg); err != nil {
		return nil, fmt.Errorf("unmarshal message error: %w", err)
	}

	return &msg, nil
}

func handleElementMsg(cli *op.Client, msg *pb.Message) error {
	timeDiff := calculateTimeDiff(msg.Timestamp)

	switch msg.Type {
	case pb.MessageType_CHAT:
		return handleChatMessage(cli, msg.GetChatContent())
	case pb.MessageType_STATUS:
		return handleStatusMessage(cli, msg, timeDiff)
	case pb.MessageType_SYNC:
		return handleSyncMessage(cli)
	case pb.MessageType_EXPIRED:
		return handleExpiredMessage(cli, msg.GetExpirationId())
	case pb.MessageType_CHECK_STATUS:
		return handleCheckStatusMessage(cli, msg, timeDiff)
	case pb.MessageType_WEBRTC_OFFER:
		return handleWebRTCOffer(cli, msg.GetWebrtcData())
	case pb.MessageType_WEBRTC_ANSWER:
		return handleWebRTCAnswer(cli, msg.GetWebrtcData())
	case pb.MessageType_WEBRTC_ICE_CANDIDATE:
		return handleWebRTCIceCandidate(cli, msg.GetWebrtcData())
	case pb.MessageType_WEBRTC_JOIN:
		return handleWebRTCJoin(cli)
	case pb.MessageType_WEBRTC_LEAVE:
		return handleWebRTCLeave(cli)
	default:
		return sendErrorMessage(cli, fmt.Sprintf("unknown message type: %v", msg.Type))
	}
}

func handleWebRTCOffer(cli *op.Client, data *pb.WebRTCData) error {
	if !cli.User().HasRoomWebRTCPermission(cli.Room()) {
		return sendErrorMessage(cli, "no permission to send webrtc offer")
	}

	if data == nil {
		return sendErrorMessage(cli, "webrtc data is nil")
	}

	sp := strings.Split(data.To, ":")
	if len(sp) != 2 {
		return sendErrorMessage(cli, "target user id is invalid")
	}

	data.From = fmt.Sprintf("%s:%s", cli.User().ID, cli.ConnID())

	return cli.Room().SendToConnID(sp[0], sp[1], &pb.Message{
		Type: pb.MessageType_WEBRTC_OFFER,
		Sender: &pb.Sender{
			UserId:   cli.User().ID,
			Username: cli.User().Username,
		},
		Payload: &pb.Message_WebrtcData{
			WebrtcData: data,
		},
	})
}

func handleWebRTCAnswer(cli *op.Client, data *pb.WebRTCData) error {
	if !cli.User().HasRoomWebRTCPermission(cli.Room()) {
		return sendErrorMessage(cli, "no permission to send webrtc answer")
	}

	if data == nil {
		return sendErrorMessage(cli, "webrtc data is nil")
	}

	sp := strings.Split(data.To, ":")
	if len(sp) != 2 {
		return sendErrorMessage(cli, "target user id is invalid")
	}

	data.From = fmt.Sprintf("%s:%s", cli.User().ID, cli.ConnID())

	return cli.Room().SendToConnID(sp[0], sp[1], &pb.Message{
		Type: pb.MessageType_WEBRTC_ANSWER,
		Sender: &pb.Sender{
			UserId:   cli.User().ID,
			Username: cli.User().Username,
		},
		Payload: &pb.Message_WebrtcData{
			WebrtcData: data,
		},
	})
}

func handleWebRTCIceCandidate(cli *op.Client, data *pb.WebRTCData) error {
	if !cli.User().HasRoomWebRTCPermission(cli.Room()) {
		return sendErrorMessage(cli, "no permission to send webrtc ice candidate")
	}

	if data == nil {
		return sendErrorMessage(cli, "webrtc data is nil")
	}

	sp := strings.Split(data.To, ":")
	if len(sp) != 2 {
		return sendErrorMessage(cli, "target user id is invalid")
	}

	data.From = fmt.Sprintf("%s:%s", cli.User().ID, cli.ConnID())

	return cli.Room().SendToConnID(sp[0], sp[1], &pb.Message{
		Type: pb.MessageType_WEBRTC_ICE_CANDIDATE,
		Sender: &pb.Sender{
			UserId:   cli.User().ID,
			Username: cli.User().Username,
		},
		Payload: &pb.Message_WebrtcData{
			WebrtcData: data,
		},
	})
}

func handleWebRTCJoin(cli *op.Client) error {
	if !cli.User().HasRoomWebRTCPermission(cli.Room()) {
		return sendErrorMessage(cli, "no permission to join webrtc")
	}

	cli.SetRTCJoined(true)
	return cli.Broadcast(&pb.Message{
		Type: pb.MessageType_WEBRTC_JOIN,
		Sender: &pb.Sender{
			UserId:   cli.User().ID,
			Username: cli.User().Username,
		},
		Payload: &pb.Message_WebrtcData{
			WebrtcData: &pb.WebRTCData{
				From: fmt.Sprintf("%s:%s", cli.User().ID, cli.ConnID()),
			},
		},
	}, op.WithIgnoreConnID(cli.ConnID()), op.WithRTCJoined())
}

func handleWebRTCLeave(cli *op.Client) error {
	if !cli.User().HasRoomWebRTCPermission(cli.Room()) {
		return sendErrorMessage(cli, "no permission to leave webrtc")
	}

	cli.SetRTCJoined(false)
	return cli.Broadcast(&pb.Message{
		Type: pb.MessageType_WEBRTC_LEAVE,
		Sender: &pb.Sender{
			UserId:   cli.User().ID,
			Username: cli.User().Username,
		},
		Payload: &pb.Message_WebrtcData{
			WebrtcData: &pb.WebRTCData{
				From: fmt.Sprintf("%s:%s", cli.User().ID, cli.ConnID()),
			},
		},
	}, op.WithIgnoreConnID(cli.ConnID()), op.WithRTCJoined())
}

func calculateTimeDiff(timestamp int64) float64 {
	if timestamp == 0 {
		return 0.0
	}
	timeDiff := time.Since(time.UnixMilli(timestamp)).Seconds()
	if timeDiff < 0 {
		return 0
	}
	if timeDiff > 1.5 {
		return 1.5
	}
	return timeDiff
}

func handleChatMessage(cli *op.Client, message string) error {
	if message == "" {
		return sendErrorMessage(cli, "message is empty")
	}
	if len(message) > MaxChatMessageLength {
		return sendErrorMessage(cli, "message too long")
	}
	err := cli.SendChatMessage(message)
	if err != nil && errors.Is(err, dbModel.ErrNoPermission) {
		return sendErrorMessage(cli, fmt.Sprintf("send chat message error: %v", err))
	}
	return err
}

func handleStatusMessage(cli *op.Client, msg *pb.Message, timeDiff float64) error {
	playbackStatus := msg.GetPlaybackStatus()
	if playbackStatus == nil {
		return sendErrorMessage(cli, "playback status is nil")
	}
	err := cli.SetStatus(
		playbackStatus.GetIsPlaying(),
		playbackStatus.GetCurrentTime(),
		playbackStatus.GetPlaybackRate(),
		timeDiff,
	)
	if err != nil {
		return sendErrorMessage(cli, fmt.Sprintf("set status error: %v", err))
	}
	return nil
}

func handleSyncMessage(cli *op.Client) error {
	status := cli.Room().Current().Status
	return cli.Send(&pb.Message{
		Type:      pb.MessageType_SYNC,
		Timestamp: time.Now().UnixMilli(),
		Payload: &pb.Message_PlaybackStatus{
			PlaybackStatus: &pb.Status{
				IsPlaying:    status.IsPlaying,
				CurrentTime:  status.CurrentTime,
				PlaybackRate: status.PlaybackRate,
			},
		},
	})
}

func handleExpiredMessage(cli *op.Client, expirationID uint64) error {
	current := cli.Room().Current()
	if expirationID != 0 && current.Movie.ID != "" {
		currentMovie, err := cli.Room().GetMovieByID(current.Movie.ID)
		if err != nil {
			return sendErrorMessage(cli, fmt.Sprintf("get movie by id error: %v", err))
		}
		if currentMovie.CheckExpired(expirationID) {
			return cli.Send(&pb.Message{
				Type: pb.MessageType_EXPIRED,
			})
		}
	}
	return nil
}

func handleCheckStatusMessage(cli *op.Client, msg *pb.Message, timeDiff float64) error {
	current := cli.Room().Current()
	status := current.Status
	cliStatus := msg.GetPlaybackStatus()
	if cliStatus == nil {
		return sendErrorMessage(cli, "playback status is nil")
	}
	if needsSync(cliStatus, status, timeDiff) {
		return sendSyncStatus(cli, &status)
	}
	return nil
}

func needsSync(clientStatus *pb.Status, serverStatus op.Status, timeDiff float64) bool {
	if clientStatus.IsPlaying != serverStatus.IsPlaying ||
		clientStatus.PlaybackRate != serverStatus.PlaybackRate ||
		serverStatus.CurrentTime+maxInterval < clientStatus.CurrentTime+timeDiff ||
		serverStatus.CurrentTime-maxInterval > clientStatus.CurrentTime+timeDiff {
		return true
	}
	return false
}

func sendErrorMessage(c *op.Client, errorMsg string) error {
	return c.Send(&pb.Message{
		Type: pb.MessageType_ERROR,
		Payload: &pb.Message_ErrorMessage{
			ErrorMessage: errorMsg,
		},
	})
}

func sendSyncStatus(cli *op.Client, status *op.Status) error {
	return cli.Send(&pb.Message{
		Type: pb.MessageType_CHECK_STATUS,
		Payload: &pb.Message_PlaybackStatus{
			PlaybackStatus: &pb.Status{
				IsPlaying:    status.IsPlaying,
				CurrentTime:  status.CurrentTime,
				PlaybackRate: status.PlaybackRate,
			},
		},
	})
}
