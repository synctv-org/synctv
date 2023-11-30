package op

import (
	"errors"
	"fmt"
	"sync"
	"sync/atomic"
	"time"

	"github.com/gorilla/websocket"
	log "github.com/sirupsen/logrus"
	pb "github.com/synctv-org/synctv/proto/message"
	"github.com/synctv-org/synctv/utils"
	"github.com/zijiren233/gencontainer/rwmap"
)

type Hub struct {
	id        string
	clients   rwmap.RWMap[string, *rwmap.RWMap[*Client, struct{}]]
	broadcast chan *broadcastMessage
	exit      chan struct{}
	closed    uint32
	wg        sync.WaitGroup

	once utils.Once
}

type broadcastMessage struct {
	data         Message
	ignoreClient []*Client
	ignoreId     []string
}

type BroadcastConf func(*broadcastMessage)

func WithIgnoreClient(cli ...*Client) BroadcastConf {
	return func(bm *broadcastMessage) {
		bm.ignoreClient = cli
	}
}

func WithIgnoreId(id ...string) BroadcastConf {
	return func(bm *broadcastMessage) {
		bm.ignoreId = id
	}
}

func newHub(id string) *Hub {
	return &Hub{
		id:        id,
		broadcast: make(chan *broadcastMessage, 128),
		exit:      make(chan struct{}),
	}
}

func (h *Hub) Start() error {
	h.once.Do(func() {
		go h.serve()
		go h.ping()
	})
	return nil
}

func (h *Hub) serve() error {
	for {
		select {
		case message := <-h.broadcast:
			h.devMessage(message.data)
			h.clients.Range(func(id string, cli *rwmap.RWMap[*Client, struct{}]) bool {
				cli.Range(func(c *Client, value struct{}) bool {
					if utils.In(message.ignoreId, c.u.ID) {
						return true
					}
					if utils.In(message.ignoreClient, c) {
						return true
					}
					if err := c.Send(message.data); err != nil {
						log.Debugf("hub: %s, write to client err: %s\nmessage: %+v", h.id, err, message)
						c.Close()
					}
					return true
				})

				return true
			})
		case <-h.exit:
			log.Debugf("hub: %s, closed", h.id)
			return nil
		}
	}
}

func (h *Hub) ping() {
	ticker := time.NewTicker(time.Second * 5)
	defer ticker.Stop()
	var (
		pre     int64 = 0
		current int64
	)
	for {
		select {
		case <-ticker.C:
			current = h.PeopleNum()
			if current != pre {
				if err := h.Broadcast(&ElementMessage{
					Type:      pb.ElementMessageType_CHANGE_PEOPLE,
					PeopleNum: current,
				}); err != nil {
					continue
				}
				pre = current
			} else {
				if err := h.Broadcast(&PingMessage{}); err != nil {
					continue
				}
			}
		case <-h.exit:
			return
		}
	}
}

func (h *Hub) devMessage(msg Message) {
	switch msg.MessageType() {
	case websocket.TextMessage:
		log.Debugf("hub: %s, broadcast:\nmessage: %+v", h.id, msg.String())
	}
}

func (h *Hub) Closed() bool {
	return atomic.LoadUint32(&h.closed) == 1
}

var (
	ErrAlreadyClosed = fmt.Errorf("already closed")
)

func (h *Hub) Close() error {
	if !atomic.CompareAndSwapUint32(&h.closed, 0, 1) {
		return ErrAlreadyClosed
	}
	close(h.exit)
	h.clients.Range(func(id string, client *rwmap.RWMap[*Client, struct{}]) bool {
		h.clients.Delete(id)
		client.Range(func(key *Client, value struct{}) bool {
			client.Delete(key)
			key.Close()
			return true
		})
		return true
	})
	h.wg.Wait()
	close(h.broadcast)
	return nil
}

func (h *Hub) Broadcast(data Message, conf ...BroadcastConf) error {
	h.wg.Add(1)
	defer h.wg.Done()
	if h.Closed() {
		return ErrAlreadyClosed
	}
	h.once.Done()
	msg := &broadcastMessage{data: data}
	for _, c := range conf {
		c(msg)
	}
	select {
	case h.broadcast <- msg:
		return nil
	case <-h.exit:
		return ErrAlreadyClosed
	}
}

func (h *Hub) RegClient(cli *Client) error {
	if h.Closed() {
		return ErrAlreadyClosed
	}
	err := h.Start()
	if err != nil {
		return err
	}
	c, _ := h.clients.LoadOrStore(cli.u.ID, &rwmap.RWMap[*Client, struct{}]{})
	_, loaded := c.LoadOrStore(cli, struct{}{})
	if loaded {
		return errors.New("client already exist")
	}
	return nil
}

func (h *Hub) UnRegClient(cli *Client) error {
	if h.Closed() {
		return ErrAlreadyClosed
	}
	if cli == nil {
		return errors.New("user is nil")
	}
	c, loaded := h.clients.Load(cli.u.ID)
	if !loaded {
		return errors.New("client not found")
	}
	_, loaded2 := c.LoadAndDelete(cli)
	if !loaded2 {
		return errors.New("client not found")
	}
	if c.Len() == 0 {
		if h.clients.CompareAndDelete(cli.u.ID, c) {
			c.Range(func(key *Client, value struct{}) bool {
				c.Delete(key)
				h.RegClient(key)
				return true
			})
		}
	}
	return nil
}

func (h *Hub) PeopleNum() int64 {
	return h.clients.Len()
}

func (h *Hub) SendToUser(userID string, data Message) (err error) {
	if h.Closed() {
		return ErrAlreadyClosed
	}
	cli, ok := h.clients.Load(userID)
	if !ok {
		return nil
	}
	cli.Range(func(key *Client, value struct{}) bool {
		if err = key.Send(data); err != nil {
			cli.CompareAndDelete(key, value)
			log.Debugf("hub: %s, write to client err: %s\nmessage: %+v", h.id, err, data)
			key.Close()
		}
		return true
	})
	return
}

func (h *Hub) LoadClient(userID string) (*rwmap.RWMap[*Client, struct{}], bool) {
	return h.clients.Load(userID)
}
