package handlers

import (
	"net/http"
	"time"

	json "github.com/json-iterator/go"

	"github.com/gin-gonic/gin"
	"github.com/gorilla/websocket"
	log "github.com/sirupsen/logrus"
	"github.com/synctv-org/synctv/cmd/flags"
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
				Type:    room.Error,
				Message: err.Error(),
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
	var timeDiff float64
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
		case websocket.TextMessage:
			msg := room.ElementMessage{}
			if err := json.NewDecoder(rd).Decode(&msg); err != nil {
				log.Errorf("ws: room %s user %s decode message error: %v", c.Room().ID(), c.Username(), err)
				if err := c.Send(&room.ElementMessage{
					Type:    room.Error,
					Message: err.Error(),
				}); err != nil {
					log.Errorf("ws: room %s user %s send error message error: %v", c.Room().ID(), c.Username(), err)
					return err
				}
				continue
			}
			if flags.Dev {
				log.Infof("ws: receive room %s user %s message: %+v", c.Room().ID(), c.Username(), msg)
			}
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
			case room.ChatMessage:
				if len(msg.Message) > 4096 {
					c.Send(&room.ElementMessage{
						Type:    room.Error,
						Message: "message too long",
					})
					continue
				}
				c.Broadcast(&room.ElementMessage{
					Type:    room.ChatMessage,
					Sender:  c.Username(),
					Message: msg.Message,
				}, room.WithSendToSelf())
			case room.Play:
				status := c.Room().SetStatus(true, msg.Seek, msg.Rate, timeDiff)
				c.Broadcast(&room.ElementMessage{
					Type:   room.Play,
					Sender: c.Username(),
					Seek:   status.Seek,
					Rate:   status.Rate,
				})
			case room.Pause:
				status := c.Room().SetStatus(false, msg.Seek, msg.Rate, timeDiff)
				c.Broadcast(&room.ElementMessage{
					Type:   room.Pause,
					Sender: c.Username(),
					Seek:   status.Seek,
					Rate:   status.Rate,
				})
			case room.ChangeRate:
				status := c.Room().SetSeekRate(msg.Seek, msg.Rate, timeDiff)
				c.Broadcast(&room.ElementMessage{
					Type:   room.ChangeRate,
					Sender: c.Username(),
					Seek:   status.Seek,
					Rate:   status.Rate,
				})
			case room.ChangeSeek:
				status := c.Room().SetSeekRate(msg.Seek, msg.Rate, timeDiff)
				c.Broadcast(&room.ElementMessage{
					Type:   room.ChangeSeek,
					Sender: c.Username(),
					Seek:   status.Seek,
					Rate:   status.Rate,
				})
			case room.CheckSeek:
				status := c.Room().Current().Status()
				if status.Seek+maxInterval < msg.Seek+timeDiff {
					c.Send(&room.ElementMessage{
						Type: room.TooFast,
						Seek: status.Seek,
						Rate: status.Rate,
					})
				} else if status.Seek-maxInterval > msg.Seek+timeDiff {
					c.Send(&room.ElementMessage{
						Type: room.TooSlow,
						Seek: status.Seek,
						Rate: status.Rate,
					})
				} else {
					c.Send(&room.ElementMessage{
						Type: room.CheckSeek,
						Seek: status.Seek,
						Rate: status.Rate,
					})
				}
			}
		}
	}
}
