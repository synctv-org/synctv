package op

import (
	"io"

	json "github.com/json-iterator/go"

	"github.com/gorilla/websocket"
	pb "github.com/synctv-org/synctv/proto/message"
	"google.golang.org/protobuf/proto"
)

type Message interface {
	MessageType() int
	String() string
	Encode(w io.Writer) error
	BeforeSend(sendTo *User) error
}

type ElementJsonMessage struct {
	BeforeSendFunc func(sendTo *User) error
	*pb.ElementMessage
}

func (em *ElementJsonMessage) MessageType() int {
	return websocket.TextMessage
}

func (em *ElementJsonMessage) String() string {
	return em.ElementMessage.String()
}

func (em *ElementJsonMessage) Encode(w io.Writer) error {
	return json.NewEncoder(w).Encode(em)
}

func (em *ElementJsonMessage) BeforeSend(sendTo *User) error {
	if em.BeforeSendFunc != nil {
		return em.BeforeSendFunc(sendTo)
	}
	return nil
}

type ElementMessage struct {
	BeforeSendFunc func(sendTo *User) error
	*pb.ElementMessage
}

func (em *ElementMessage) MessageType() int {
	return websocket.BinaryMessage
}

func (em *ElementMessage) String() string {
	return em.ElementMessage.String()
}

func (em *ElementMessage) Encode(w io.Writer) error {
	b, err := proto.Marshal(em)
	if err != nil {
		return err
	}
	_, err = w.Write(b)
	return err
}

func (em *ElementMessage) BeforeSend(sendTo *User) error {
	if em.BeforeSendFunc != nil {
		return em.BeforeSendFunc(sendTo)
	}
	return nil
}

type PingMessage struct{}

func (pm *PingMessage) MessageType() int {
	return websocket.PingMessage
}

func (pm *PingMessage) String() string {
	return "Ping"
}

func (pm *PingMessage) Encode(w io.Writer) error {
	return nil
}

func (pm *PingMessage) BeforeSend(sendTo *User) error {
	return nil
}
