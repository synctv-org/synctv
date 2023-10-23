package handlers

import (
	"io"
	"net/http"
	"time"

	"github.com/gin-gonic/gin"
	"github.com/gorilla/websocket"
	json "github.com/json-iterator/go"
	log "github.com/sirupsen/logrus"
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
		user, room, err := middlewares.AuthRoom(token)
		if err != nil {
			ctx.AbortWithStatusJSON(http.StatusUnauthorized, model.NewApiErrorResp(err))
			return
		}

		wss.Server(ctx.Writer, ctx.Request, []string{token}, NewWSMessageHandler(user, room))
	}
}

func NewWSMessageHandler(u *op.User, r *op.Room) func(c *websocket.Conn) error {
	return func(c *websocket.Conn) error {
		client, err := r.RegClient(u, c)
		if err != nil {
			log.Errorf("ws: register client error: %v", err)
			wc, err2 := c.NextWriter(websocket.BinaryMessage)
			if err2 != nil {
				return err2
			}
			defer wc.Close()
			em := op.ElementMessage{
				ElementMessage: &pb.ElementMessage{
					Type:    pb.ElementMessageType_ERROR,
					Message: err.Error(),
				},
			}
			return em.Encode(wc)
		}
		log.Infof("ws: room %s user %s connected", r.Name, u.Username)
		defer func() {
			r.UnregisterClient(u)
			client.Close()
			log.Infof("ws: room %s user %s disconnected", r.Name, u.Username)
		}()
		go handleReaderMessage(client)
		return handleWriterMessage(client)
	}
}

func handleWriterMessage(c *op.Client) error {
	for v := range c.GetReadChan() {
		wc, err := c.NextWriter(v.MessageType())
		if err != nil {
			log.Debugf("ws: room %s user %s get next writer error: %v", c.Room().Name, c.User().Username, err)
			return err
		}

		if err := v.Encode(wc); err != nil {
			log.Debugf("ws: room %s user %s encode message error: %v", c.Room().Name, c.User().Username, err)
			continue
		}
		if err := wc.Close(); err != nil {
			return err
		}
	}
	return nil
}

func handleReaderMessage(c *op.Client) error {
	defer c.Close()
	var msg pb.ElementMessage
	for {
		t, rd, err := c.NextReader()
		if err != nil {
			log.Debugf("ws: room %s user %s get next reader error: %v", c.Room().Name, c.User().Username, err)
			return err
		}
		log.Debugf("ws: room %s user %s receive message type: %d", c.Room().Name, c.User().Username, t)
		switch t {
		case websocket.CloseMessage:
			log.Debugf("ws: room %s user %s receive close message", c.Room().Name, c.User().Username)
			return nil
		case websocket.BinaryMessage:
			var data []byte
			if data, err = io.ReadAll(rd); err != nil {
				log.Errorf("ws: room %s user %s read message error: %v", c.Room().Name, c.User().Username, err)
				if err := c.Send(&op.ElementMessage{
					ElementMessage: &pb.ElementMessage{
						Type:    pb.ElementMessageType_ERROR,
						Message: err.Error(),
					},
				}); err != nil {
					log.Errorf("ws: room %s user %s send error message error: %v", c.Room().Name, c.User().Username, err)
					return err
				}
				continue
			}
			if err := proto.Unmarshal(data, &msg); err != nil {
				log.Errorf("ws: room %s user %s decode message error: %v", c.Room().Name, c.User().Username, err)
				if err := c.Send(&op.ElementMessage{
					ElementMessage: &pb.ElementMessage{
						Type:    pb.ElementMessageType_ERROR,
						Message: err.Error(),
					},
				}); err != nil {
					log.Errorf("ws: room %s user %s send error message error: %v", c.Room().Name, c.User().Username, err)
					return err
				}
				continue
			}
		case websocket.TextMessage:
			if err := json.NewDecoder(rd).Decode(&msg); err != nil {
				log.Errorf("ws: room %s user %s decode message error: %v", c.Room().Name, c.User().Username, err)
				if err := c.Send(&op.ElementMessage{
					ElementMessage: &pb.ElementMessage{
						Type:    pb.ElementMessageType_ERROR,
						Message: err.Error(),
					},
				}); err != nil {
					log.Errorf("ws: room %s user %s send error message error: %v", c.Room().Name, c.User().Username, err)
					return err
				}
				continue
			}
		default:
			log.Errorf("ws: room %s user %s receive unknown message type: %d", c.Room().Name, c.User().Username, t)
			continue
		}
		log.Debugf("ws: receive room %s user %s message: %+v", c.Room().Name, c.User().Username, msg.String())
		switch t {
		case websocket.BinaryMessage:
			err = handleElementMsg(c.Room(), &msg, func(em *pb.ElementMessage) error {
				em.Sender = c.User().Username
				return c.Send(&op.ElementMessage{ElementMessage: em})
			}, func(em *pb.ElementMessage, bc ...op.BroadcastConf) error {
				em.Sender = c.User().Username
				return c.Broadcast(&op.ElementMessage{ElementMessage: em}, bc...)
			})
		case websocket.TextMessage:
			err = handleElementMsg(c.Room(), &msg, func(em *pb.ElementMessage) error {
				em.Sender = c.User().Username
				return c.Send(&op.ElementJsonMessage{ElementMessage: em})
			}, func(em *pb.ElementMessage, bc ...op.BroadcastConf) error {
				em.Sender = c.User().Username
				return c.Broadcast(&op.ElementJsonMessage{ElementMessage: em}, bc...)
			})
		}
		if err != nil {
			log.Errorf("ws: room %s user %s handle message error: %v", c.Room().Name, c.User().Username, err)
			return err
		}
	}
}

type send func(*pb.ElementMessage) error

type broadcast func(*pb.ElementMessage, ...op.BroadcastConf) error

func handleElementMsg(r *op.Room, msg *pb.ElementMessage, send send, broadcast broadcast) error {
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
		if len(msg.Message) > 4096 {
			send(&pb.ElementMessage{
				Type:    pb.ElementMessageType_ERROR,
				Message: "message too long",
			})
			return nil
		}
		broadcast(&pb.ElementMessage{
			Type:    pb.ElementMessageType_CHAT_MESSAGE,
			Message: msg.Message,
		}, op.WithSendToSelf())
	case pb.ElementMessageType_PLAY:
		status := r.SetStatus(true, msg.Seek, msg.Rate, timeDiff)
		broadcast(&pb.ElementMessage{
			Type: pb.ElementMessageType_PLAY,
			Seek: status.Seek,
			Rate: status.Rate,
		})
	case pb.ElementMessageType_PAUSE:
		status := r.SetStatus(false, msg.Seek, msg.Rate, timeDiff)
		broadcast(&pb.ElementMessage{
			Type: pb.ElementMessageType_PAUSE,
			Seek: status.Seek,
			Rate: status.Rate,
		})
	case pb.ElementMessageType_CHANGE_RATE:
		status := r.SetSeekRate(msg.Seek, msg.Rate, timeDiff)
		broadcast(&pb.ElementMessage{
			Type: pb.ElementMessageType_CHANGE_RATE,
			Seek: status.Seek,
			Rate: status.Rate,
		})
	case pb.ElementMessageType_CHANGE_SEEK:
		status := r.SetSeekRate(msg.Seek, msg.Rate, timeDiff)
		broadcast(&pb.ElementMessage{
			Type: pb.ElementMessageType_CHANGE_SEEK,
			Seek: status.Seek,
			Rate: status.Rate,
		})
	case pb.ElementMessageType_CHECK_SEEK:
		status := r.Current().Status
		if status.Seek+maxInterval < msg.Seek+timeDiff {
			send(&pb.ElementMessage{
				Type: pb.ElementMessageType_TOO_FAST,
				Seek: status.Seek,
				Rate: status.Rate,
			})
		} else if status.Seek-maxInterval > msg.Seek+timeDiff {
			send(&pb.ElementMessage{
				Type: pb.ElementMessageType_TOO_SLOW,
				Seek: status.Seek,
				Rate: status.Rate,
			})
		} else {
			send(&pb.ElementMessage{
				Type: pb.ElementMessageType_CHECK_SEEK,
				Seek: status.Seek,
				Rate: status.Rate,
			})
		}
	}
	return nil
}
