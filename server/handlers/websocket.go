package handlers

import (
	"errors"
	"fmt"
	"io"
	"net/http"
	"time"

	"github.com/gin-gonic/gin"
	"github.com/gorilla/websocket"
	"github.com/sirupsen/logrus"
	log "github.com/sirupsen/logrus"
	dbModel "github.com/synctv-org/synctv/internal/model"
	"github.com/synctv-org/synctv/internal/op"
	pb "github.com/synctv-org/synctv/proto/message"
	"github.com/synctv-org/synctv/server/middlewares"
	"github.com/synctv-org/synctv/server/model"
	"github.com/synctv-org/synctv/utils"
	"google.golang.org/protobuf/proto"
)

const maxInterval = 10

func NewWebSocketHandler(wss *utils.WebSocket) gin.HandlerFunc {
	return func(ctx *gin.Context) {
		token := ctx.GetHeader("Sec-WebSocket-Protocol")
		userE, roomE, err := middlewares.AuthRoom(token)
		if err != nil {
			ctx.AbortWithStatusJSON(http.StatusUnauthorized, model.NewApiErrorResp(err))
			return
		}
		user := userE.Value()
		room := roomE.Value()
		entry := log.WithFields(log.Fields{
			"rid": room.ID,
			"rnm": room.Name,
			"uid": user.ID,
			"unm": user.Username,
			"uro": user.Role.String(),
		})

		_ = wss.Server(ctx.Writer, ctx.Request, []string{token}, NewWSMessageHandler(user, room, entry))
	}
}

func NewWSMessageHandler(u *op.User, r *op.Room, l *logrus.Entry) func(c *websocket.Conn) error {
	return func(c *websocket.Conn) error {
		client, err := r.NewClient(u, c)
		if err != nil {
			log.Errorf("ws: register client error: %v", err)
			wc, err2 := c.NextWriter(websocket.BinaryMessage)
			if err2 != nil {
				return err2
			}
			defer wc.Close()
			em := pb.ElementMessage{
				Type:  pb.ElementMessageType_ERROR,
				Error: err.Error(),
			}
			return em.Encode(wc)
		}
		l.Info("ws: connected")
		defer func() {
			_ = r.UnregisterClient(client)
			client.Close()
			l.Info("ws: disconnected")
		}()
		go handleReaderMessage(client, l)
		return handleWriterMessage(client, l)
	}
}

func handleWriterMessage(c *op.Client, l *logrus.Entry) error {
	for v := range c.GetReadChan() {
		wc, err := c.NextWriter(v.MessageType())
		if err != nil {
			l.Errorf("ws: get next writer error: %v", err)
			return err
		}

		if err = v.Encode(wc); err != nil {
			l.Errorf("ws: encode message error: %v", err)
			return err
		}

		if err = wc.Close(); err != nil {
			l.Errorf("ws: close writer error: %v", err)
			return err
		}
	}
	return nil
}

func handleReaderMessage(c *op.Client, l *logrus.Entry) error {
	defer func() {
		c.Close()
		if r := recover(); r != nil {
			l.Errorf("ws: panic: %v", r)
		}
	}()
	for {
		t, rd, err := c.NextReader()
		if err != nil {
			l.Errorf("ws: get next reader error: %v", err)
			return err
		}
		l.Debugf("ws: receive message type: %d", t)
		if t != websocket.BinaryMessage {
			l.Errorf("ws: receive unknown message type: %d", t)
			continue
		}
		var data []byte
		if data, err = io.ReadAll(rd); err != nil {
			l.Errorf("ws: read message error: %v", err)
			if err := c.Send(&pb.ElementMessage{
				Type:  pb.ElementMessageType_ERROR,
				Error: err.Error(),
			}); err != nil {
				l.Errorf("ws: send error message error: %v", err)
				return err
			}
			continue
		}
		var msg pb.ElementMessage
		if err := proto.Unmarshal(data, &msg); err != nil {
			l.Errorf("ws: unmarshal message error: %v", err)
			if err := c.Send(&pb.ElementMessage{
				Type:  pb.ElementMessageType_ERROR,
				Error: err.Error(),
			}); err != nil {
				l.Errorf("ws: send error message error: %v", err)
				return err
			}
			continue
		}

		l.Debugf("ws: receive message: %v", msg.String())
		if err = handleElementMsg(c, &msg); err != nil {
			l.Errorf("ws: handle message error: %v", err)
			return err
		}
	}
}

const MaxChatMessageLength = 4096

func handleElementMsg(cli *op.Client, msg *pb.ElementMessage) error {
	var timeDiff float64
	if msg.Time != 0 {
		timeDiff = time.Since(time.UnixMilli(msg.Time)).Seconds()
	} else {
		timeDiff = 0.0
	}
	if timeDiff < 0 {
		timeDiff = 0
	} else if timeDiff > 1.5 {
		timeDiff = 1.5
	}
	switch msg.Type {
	case pb.ElementMessageType_CHAT_MESSAGE:
		message := msg.GetChatReq()
		if len(message) > MaxChatMessageLength {
			return cli.Send(&pb.ElementMessage{
				Type:  pb.ElementMessageType_ERROR,
				Error: "message too long",
			})
		}
		err := cli.SendChatMessage(message)
		if err != nil && errors.Is(err, dbModel.ErrNoPermission) {
			return cli.Send(&pb.ElementMessage{
				Type:  pb.ElementMessageType_ERROR,
				Error: fmt.Sprintf("send chat message error: %v", err),
			})
		}
		return nil
	case pb.ElementMessageType_PLAY,
		pb.ElementMessageType_PAUSE,
		pb.ElementMessageType_CHANGE_RATE:
		status, err := cli.SetStatus(msg.ChangeMovieStatusReq.Playing, msg.ChangeMovieStatusReq.Seek, msg.ChangeMovieStatusReq.Rate, timeDiff)
		if err != nil {
			return cli.Send(&pb.ElementMessage{
				Type:  pb.ElementMessageType_ERROR,
				Error: fmt.Sprintf("set status error: %v", err),
			})
		}
		return cli.Broadcast(&pb.ElementMessage{
			Type: msg.Type,
			MovieStatusChanged: &pb.MovieStatusChanged{
				Sender: &pb.Sender{
					Username: cli.User().Username,
					Userid:   cli.User().ID,
				},
				Status: &pb.MovieStatus{
					Playing: status.Playing,
					Seek:    status.Seek,
					Rate:    status.Rate,
				},
			},
		}, op.WithIgnoreClient(cli))
	case pb.ElementMessageType_CHANGE_SEEK:
		status, err := cli.SetSeekRate(msg.ChangeMovieStatusReq.Seek, msg.ChangeMovieStatusReq.Rate, timeDiff)
		if err != nil {
			return cli.Send(&pb.ElementMessage{
				Type:  pb.ElementMessageType_ERROR,
				Error: fmt.Sprintf("set seek rate error: %v", err),
			})
		}
		return cli.Broadcast(&pb.ElementMessage{
			Type: msg.Type,
			MovieStatusChanged: &pb.MovieStatusChanged{
				Sender: &pb.Sender{
					Username: cli.User().Username,
					Userid:   cli.User().ID,
				},
				Status: &pb.MovieStatus{
					Playing: status.Playing,
					Seek:    status.Seek,
					Rate:    status.Rate,
				},
			},
		}, op.WithIgnoreClient(cli))
	case pb.ElementMessageType_SYNC_MOVIE_STATUS:
		status := cli.Room().Current().Status
		return cli.Send(&pb.ElementMessage{
			Type: pb.ElementMessageType_SYNC_MOVIE_STATUS,
			MovieStatusChanged: &pb.MovieStatusChanged{
				Sender: &pb.Sender{
					Username: cli.User().Username,
					Userid:   cli.User().ID,
				},
				Status: &pb.MovieStatus{
					Playing: status.Playing,
					Seek:    status.Seek,
					Rate:    status.Rate,
				},
			},
		})
	case pb.ElementMessageType_CHECK:
		current := cli.Room().Current()
		if msg.CheckReq.ExpireId != 0 && current.MovieID != "" {
			currentMovie, err := cli.Room().GetMovieByID(current.MovieID)
			if err != nil {
				return cli.Send(&pb.ElementMessage{
					Type:  pb.ElementMessageType_ERROR,
					Error: fmt.Sprintf("get movie by id error: %v", err),
				})
			}
			if currentMovie.CheckExpired(msg.CheckReq.ExpireId) {
				return cli.Send(&pb.ElementMessage{
					Type: pb.ElementMessageType_CURRENT_CHANGED,
				})
			}
		}
		status := current.Status
		cliStatus := msg.CheckReq.Status
		if status.Seek+maxInterval < cliStatus.Seek+timeDiff {
			return cli.Send(&pb.ElementMessage{
				Type: pb.ElementMessageType_TOO_FAST,
				MovieStatusChanged: &pb.MovieStatusChanged{
					Status: &pb.MovieStatus{
						Playing: status.Playing,
						Seek:    status.Seek,
						Rate:    status.Rate,
					},
				},
			})
		} else if status.Seek-maxInterval > cliStatus.Seek+timeDiff {
			return cli.Send(&pb.ElementMessage{
				Type: pb.ElementMessageType_TOO_SLOW,
				MovieStatusChanged: &pb.MovieStatusChanged{
					Status: &pb.MovieStatus{
						Playing: status.Playing,
						Seek:    status.Seek,
						Rate:    status.Rate,
					},
				},
			})
		}
	}
	return nil
}
