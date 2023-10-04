package room

import (
	"errors"
	"io"
	"sync"
	"sync/atomic"
	"time"

	"github.com/gorilla/websocket"
)

type Client struct {
	user    *User
	c       chan Message
	wg      sync.WaitGroup
	conn    *websocket.Conn
	timeOut time.Duration
	closed  uint64
}

func NewClient(user *User, conn *websocket.Conn) (*Client, error) {
	if user == nil {
		return nil, errors.New("user is nil")
	}
	if conn == nil {
		return nil, errors.New("conn is nil")
	}
	return &Client{
		user:    user,
		c:       make(chan Message, 128),
		conn:    conn,
		timeOut: 10 * time.Second,
	}, nil
}

func (c *Client) User() *User {
	return c.user
}

func (c *Client) Username() string {
	return c.user.name
}

func (c *Client) Room() *Room {
	return c.user.room
}

func (c *Client) Broadcast(msg Message, conf ...BroadcastConf) {
	c.user.Broadcast(msg, conf...)
}

func (c *Client) Send(msg Message) error {
	c.wg.Add(1)
	defer c.wg.Done()
	if c.Closed() {
		return ErrAlreadyClosed
	}
	c.c <- msg
	return nil
}

func (c *Client) Unregister() error {
	return c.user.room.UnRegClient(c.user)
}

func (c *Client) Close() error {
	if !atomic.CompareAndSwapUint64(&c.closed, 0, 1) {
		return ErrAlreadyClosed
	}
	c.wg.Wait()
	close(c.c)
	return nil
}

func (c *Client) Closed() bool {
	return atomic.LoadUint64(&c.closed) == 1
}

func (c *Client) GetReadChan() <-chan Message {
	return c.c
}

func (c *Client) NextWriter(messageType int) (io.WriteCloser, error) {
	return c.conn.NextWriter(messageType)
}

func (c *Client) NextReader() (int, io.Reader, error) {
	return c.conn.NextReader()
}
