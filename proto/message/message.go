package pb

import (
	"io"

	"github.com/gorilla/websocket"
	"google.golang.org/protobuf/proto"
)

func (em *Message) MessageType() int {
	return websocket.BinaryMessage
}

func (em *Message) Encode(w io.Writer) error {
	b, err := proto.Marshal((*Message)(em))
	if err != nil {
		return err
	}
	_, err = w.Write(b)
	return err
}
