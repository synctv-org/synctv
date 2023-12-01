package op

import (
	"io"

	"github.com/gorilla/websocket"
	pb "github.com/synctv-org/synctv/proto/message"
	"google.golang.org/protobuf/proto"
)

type Message interface {
	MessageType() int
	String() string
	Encode(w io.Writer) error
}

type ElementMessage pb.ElementMessage

func (em *ElementMessage) MessageType() int {
	return websocket.BinaryMessage
}

func (em *ElementMessage) String() string {
	return (*pb.ElementMessage)(em).String()
}

func (em *ElementMessage) Encode(w io.Writer) error {
	b, err := proto.Marshal((*pb.ElementMessage)(em))
	if err != nil {
		return err
	}
	_, err = w.Write(b)
	return err
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
