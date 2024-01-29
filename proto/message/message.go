package pb

import (
	"io"

	"github.com/gorilla/websocket"
	"google.golang.org/protobuf/proto"
)

func (em *ElementMessage) MessageType() int {
	return websocket.BinaryMessage
}

func (em *ElementMessage) Encode(w io.Writer) error {
	b, err := proto.Marshal((*ElementMessage)(em))
	if err != nil {
		return err
	}
	_, err = w.Write(b)
	return err
}
