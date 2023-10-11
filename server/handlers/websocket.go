package handlers

import (
	"io"
	"net/http"
	"time"

	json "github.com/json-iterator/go"
	"google.golang.org/protobuf/proto"

	"github.com/gin-gonic/gin"
	"github.com/gorilla/websocket"
	log "github.com/sirupsen/logrus"
	"github.com/synctv-org/synctv/cmd/flags"
	pb "github.com/synctv-org/synctv/proto"
	"github.com/synctv-org/synctv/room"
	"github.com/synctv-org/synctv/utils"
)

const maxInterval = 10

func NewWebSocketHandler(wss *utils.WebSocket) gin.HandlerFunc {
	return func(ctx *gin.Context) {
		token := ctx.GetHeader("Sec-WebSocket-Protocol")
		user, err := AuthRoom(token)
		if err != nil {
			ctx.AbortWithStatusJSON(http.StatusUnauthorized, NewApiErrorResp(err))
			return
		}
		wss.Server(ctx.Writer, ctx.Request, []string{token}, NewWSMessageHandler(user))
	}
}

func NewWSMessageHandler(u *room.User) func(c *websocket.Conn) error {
	return func(c *websocket.Conn) error {
		client, err := u.RegClient(c)
		if err != nil {
			log.Errorf("ws: register client error: %v", err)
			b, err := json.Marshal(room.ElementMessage{
				ElementMessage: &pb.ElementMessage{
					Type:    pb.ElementMessageType_ERROR,
					Message: err.Error(),
				},
			})
			if err != nil {
				return err
			}
			return c.WriteMessage(websocket.TextMessage, b)
		}
		log.Infof("ws: room %s user %s connected", u.Room().ID(), u.Name())
		defer func() {
			client.Unregister()
			client.Close()
			log.Infof("ws: room %s user %s disconnected", u.Room().ID(), u.Name())
		}()
		go handleReaderMessage(client)
		return handleWriterMessage(client)
	}
}

func handleWriterMessage(c *room.Client) error {
	for v := range c.GetReadChan() {
		wc, err := c.NextWriter(v.MessageType())
		if err != nil {
			if flags.Dev {
				log.Errorf("ws: room %s user %s get next writer error: %v", c.Room().ID(), c.Username(), err)
			}
			return err
		}

		if err := v.Encode(wc); err != nil {
			if flags.Dev {
				log.Errorf("ws: room %s user %s encode message error: %v", c.Room().ID(), c.Username(), err)
			}
			continue
		}
		if err := wc.Close(); err != nil {
			return err
		}
	}
	return nil
}

func handleReaderMessage(c *room.Client) error {
	defer c.Close()
	var msg pb.ElementMessage
	for {
		t, rd, err := c.NextReader()
		if err != nil {
			if flags.Dev {
				log.Errorf("ws: room %s user %s get next reader error: %v", c.Room().ID(), c.Username(), err)
			}
			return err
		}
		log.Infof("ws: room %s user %s receive message type: %d", c.Room().ID(), c.Username(), t)
		switch t {
		case websocket.CloseMessage:
			if flags.Dev {
				log.Infof("ws: room %s user %s receive close message", c.Room().ID(), c.Username())
			}
			return nil
		case websocket.BinaryMessage:
			var data []byte
			if data, err = io.ReadAll(rd); err != nil {
				log.Errorf("ws: room %s user %s read message error: %v", c.Room().ID(), c.Username(), err)
				if err := c.Send(&room.ElementMessage{
					ElementMessage: &pb.ElementMessage{
						Type:    pb.ElementMessageType_ERROR,
						Message: err.Error(),
					},
				}); err != nil {
					log.Errorf("ws: room %s user %s send error message error: %v", c.Room().ID(), c.Username(), err)
					return err
				}
				continue
			}
			if err := proto.Unmarshal(data, &msg); err != nil {
				log.Errorf("ws: room %s user %s decode message error: %v", c.Room().ID(), c.Username(), err)
				if err := c.Send(&room.ElementMessage{
					ElementMessage: &pb.ElementMessage{
						Type:    pb.ElementMessageType_ERROR,
						Message: err.Error(),
					},
				}); err != nil {
					log.Errorf("ws: room %s user %s send error message error: %v", c.Room().ID(), c.Username(), err)
					return err
				}
				continue
			}
		case websocket.TextMessage:
			if err := json.NewDecoder(rd).Decode(&msg); err != nil {
				log.Errorf("ws: room %s user %s decode message error: %v", c.Room().ID(), c.Username(), err)
				if err := c.Send(&room.ElementMessage{
					ElementMessage: &pb.ElementMessage{
						Type:    pb.ElementMessageType_ERROR,
						Message: err.Error(),
					},
				}); err != nil {
					log.Errorf("ws: room %s user %s send error message error: %v", c.Room().ID(), c.Username(), err)
					return err
				}
				continue
			}
		}
		if flags.Dev {
			log.Infof("ws: receive room %s user %s message: %+v", c.Room().ID(), c.Username(), msg.String())
		}
		if err := handleElementMsg(c.Room(), &msg, func(em *pb.ElementMessage) error {
			em.Sender = c.Username()
			return c.Send(&room.ElementMessage{ElementMessage: em})
		}, func(em *pb.ElementMessage, bc ...room.BroadcastConf) error {
			em.Sender = c.Username()
			return c.Broadcast(&room.ElementMessage{ElementMessage: em}, bc...)
		}); err != nil {
			log.Errorf("ws: room %s user %s handle message error: %v", c.Room().ID(), c.Username(), err)
			return err
		}
	}
}

type send func(*pb.ElementMessage) error

type broadcast func(*pb.ElementMessage, ...room.BroadcastConf) error

func handleElementMsg(r *room.Room, msg *pb.ElementMessage, send send, broadcast broadcast) error {
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
		}, room.WithSendToSelf())
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
