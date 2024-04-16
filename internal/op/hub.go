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

type clients struct {
	lock sync.RWMutex
	m    map[*Client]struct{}
}

type Hub struct {
	id        string
	clients   rwmap.RWMap[string, *clients]
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
			h.clients.Range(func(id string, clients *clients) bool {
				clients.lock.RLock()
				defer clients.lock.RUnlock()
				for c := range clients.m {
					if utils.In(message.ignoreId, c.u.ID) {
						continue
					}
					if utils.In(message.ignoreClient, c) {
						continue
					}
					if err := c.Send(message.data); err != nil {
						c.Close()
					}
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
	var (
		pre     int64 = 0
		current int64
	)
	for {
		select {
		case <-ticker.C:
			current = h.PeopleNum()
			if current != pre {
				if err := h.Broadcast(&pb.ElementMessage{
					Type:          pb.ElementMessageType_PEOPLE_CHANGED,
					PeopleChanged: current,
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
	case websocket.BinaryMessage:
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
	h.clients.Range(func(id string, clients *clients) bool {
		h.clients.Delete(id)
		for c := range clients.m {
			delete(clients.m, c)
			c.Close()
		}
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
	c, _ := h.clients.LoadOrStore(cli.u.ID, &clients{})
	c.lock.Lock()
	defer c.lock.Unlock()
	if c.m == nil {
		c.m = make(map[*Client]struct{})
	} else if _, ok := c.m[cli]; ok {
		return errors.New("client already exists")
	}
	c.m[cli] = struct{}{}
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
	c.lock.Lock()
	defer c.lock.Unlock()
	if _, ok := c.m[cli]; !ok {
		return errors.New("client not found")
	}
	delete(c.m, cli)
	if len(c.m) == 0 {
		h.clients.CompareAndDelete(cli.u.ID, c)
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
	cli.lock.RLock()
	defer cli.lock.RUnlock()
	for c := range cli.m {
		if err = c.Send(data); err != nil {
			c.Close()
		}
	}
	return
}

func (h *Hub) IsOnline(userID string) bool {
	_, ok := h.clients.Load(userID)
	return ok
}

func (h *Hub) KickUser(userID string) error {
	if h.Closed() {
		return ErrAlreadyClosed
	}
	cli, ok := h.clients.Load(userID)
	if !ok {
		return nil
	}
	cli.lock.RLock()
	defer cli.lock.RUnlock()
	for c := range cli.m {
		c.Close()
	}
	return nil
}
