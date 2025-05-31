package op

import (
	"io"

	"github.com/gorilla/websocket"
)

type Message interface {
	MessageType() int
	String() string
	Encode(w io.Writer) error
}

type PingMessage struct{}

func (pm *PingMessage) MessageType() int {
	return websocket.PingMessage
}

func (pm *PingMessage) String() string {
	return "Ping"
}

func (pm *PingMessage) Encode(_ io.Writer) error {
	return nil
}
