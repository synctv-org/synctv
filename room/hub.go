package room

import (
	"errors"
	"fmt"
	"sync"
	"sync/atomic"

	"github.com/gorilla/websocket"
	log "github.com/sirupsen/logrus"
	"github.com/synctv-org/synctv/utils"
	"github.com/zijiren233/gencontainer/rwmap"
)

type hub struct {
	id        string
	clients   rwmap.RWMap[string, *Client]
	broadcast chan *broadcastMessage
	exit      chan struct{}
	closed    uint32
	wg        sync.WaitGroup
}

type broadcastMessage struct {
	data       Message
	sender     string
	sendToSelf bool
	ignoreID   []string
}

type BroadcastConf func(*broadcastMessage)

func WithSender(sender string) BroadcastConf {
	return func(bm *broadcastMessage) {
		bm.sender = sender
	}
}

func WithSendToSelf() BroadcastConf {
	return func(bm *broadcastMessage) {
		bm.sendToSelf = true
	}
}

func WithIgnoreID(id ...string) BroadcastConf {
	return func(bm *broadcastMessage) {
		bm.ignoreID = append(bm.ignoreID, id...)
	}
}

func newHub(id string) *hub {
	return &hub{
		id:        id,
		broadcast: make(chan *broadcastMessage, 128),
		clients:   rwmap.RWMap[string, *Client]{},
		exit:      make(chan struct{}),
	}
}

func (h *hub) Closed() bool {
	return atomic.LoadUint32(&h.closed) == 1
}

var (
	ErrAlreadyClosed = fmt.Errorf("already closed")
)

func (h *hub) Start() {
	go h.Serve()
}

func (h *hub) Serve() error {
	if h.Closed() {
		return ErrAlreadyClosed
	}
	for {
		select {
		case message := <-h.broadcast:
			h.devMessage(message.data)
			h.clients.Range(func(_ string, cli *Client) bool {
				if !message.sendToSelf {
					if cli.user.name == message.sender {
						return true
					}
				}
				if utils.In(message.ignoreID, cli.user.name) {
					return true
				}
				if err := cli.Send(message.data); err != nil {
					log.Debugf("hub: %s, write to client err: %s\nmessage: %+v", h.id, err, message)
					cli.Close()
				}
				return true
			})
		case <-h.exit:
			log.Debugf("hub: %s, closed", h.id)
			return nil
		}
	}
}

func (h *hub) devMessage(msg Message) {
	switch msg.MessageType() {
	case websocket.TextMessage:
		log.Debugf("hub: %s, broadcast:\nmessage: %+v", h.id, msg.String())
	}
}

func (h *hub) Close() error {
	if !atomic.CompareAndSwapUint32(&h.closed, 0, 1) {
		return ErrAlreadyClosed
	}
	close(h.exit)
	h.clients.Range(func(_ string, client *Client) bool {
		client.Close()
		return true
	})
	h.wg.Wait()
	close(h.broadcast)
	return nil
}

func (h *hub) Broadcast(data Message, conf ...BroadcastConf) error {
	h.wg.Add(1)
	defer h.wg.Done()
	if h.Closed() {
		return ErrAlreadyClosed
	}
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

func (h *hub) RegClient(user *User, conn *websocket.Conn) (*Client, error) {
	if h.Closed() {
		return nil, ErrAlreadyClosed
	}
	cli, err := NewClient(user, conn)
	if err != nil {
		return nil, err
	}
	c, loaded := h.clients.LoadOrStore(user.name, cli)
	if loaded {
		return nil, errors.New("client already registered")
	}
	return c, nil
}

func (h *hub) UnRegClient(user *User) error {
	if h.Closed() {
		return ErrAlreadyClosed
	}
	if user == nil {
		return errors.New("user is nil")
	}
	_, loaded := h.clients.LoadAndDelete(user.name)
	if !loaded {
		return errors.New("client not found")
	}
	return nil
}

func (h *hub) ClientNum() int64 {
	return h.clients.Len()
}
