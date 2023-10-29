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
	clients   rwmap.RWMap[string, *Client]
	broadcast chan *broadcastMessage
	exit      chan struct{}
	closed    uint32
	wg        sync.WaitGroup

	once utils.Once
}

type broadcastMessage struct {
	data       Message
	sender     string
	sendToSelf bool
	ignoreId   []string
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

func WithIgnoreId(id ...string) BroadcastConf {
	return func(bm *broadcastMessage) {
		bm.ignoreId = append(bm.ignoreId, id...)
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
			h.clients.Range(func(_ string, cli *Client) bool {
				if !message.sendToSelf {
					if cli.u.Username == message.sender {
						return true
					}
				}
				if utils.In(message.ignoreId, cli.u.Username) {
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

func (h *Hub) ping() {
	ticker := time.NewTicker(time.Second * 5)
	defer ticker.Stop()
	var pre int64 = 0
	for {
		select {
		case <-ticker.C:
			current := h.ClientNum()
			if current != pre {
				if err := h.Broadcast(&ElementMessage{
					ElementMessage: &pb.ElementMessage{
						Type:      pb.ElementMessageType_CHANGE_PEOPLE,
						PeopleNum: current,
					},
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
	h.clients.Range(func(_ string, client *Client) bool {
		h.clients.Delete(client.u.ID)
		client.Close()
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

func (h *Hub) RegClient(cli *Client) (*Client, error) {
	if h.Closed() {
		return nil, ErrAlreadyClosed
	}
	err := h.Start()
	if err != nil {
		return nil, err
	}
	c, loaded := h.clients.LoadOrStore(cli.u.ID, cli)
	if loaded {
		return nil, errors.New("client already registered")
	}
	return c, nil
}

func (h *Hub) UnRegClient(user *User) error {
	if h.Closed() {
		return ErrAlreadyClosed
	}
	if user == nil {
		return errors.New("user is nil")
	}
	_, loaded := h.clients.LoadAndDelete(user.ID)
	if !loaded {
		return errors.New("client not found")
	}
	return nil
}

func (h *Hub) ClientNum() int64 {
	return h.clients.Len()
}

func (h *Hub) SendToUser(userID string, data Message) error {
	if h.Closed() {
		return ErrAlreadyClosed
	}
	cli, ok := h.clients.Load(userID)
	if !ok {
		return nil
	}
	return cli.Send(data)
}
