package room

import (
	"io"

	"github.com/gorilla/websocket"
	pb "github.com/synctv-org/synctv/proto"
	"google.golang.org/protobuf/proto"
)

type Message interface {
	MessageType() int
	String() string
	Encode(wc io.Writer) error
}

type ElementMessage struct {
	*pb.ElementMessage
}

func (em *ElementMessage) MessageType() int {
	return websocket.BinaryMessage
}

func (em *ElementMessage) String() string {
	// out, _ := yaml.Marshal(em)
	// switch em.Type {
	// case Error:
	// 	return fmt.Sprintf("Element Error: %s", out)
	// case ChatMessage:
	// 	return fmt.Sprintf("Element ChatMessage: %s", out)
	// case Play:
	// 	return fmt.Sprintf("Element Play: %s", out)
	// case Pause:
	// 	return fmt.Sprintf("Element Pause: %s", out)
	// case CheckSeek:
	// 	return fmt.Sprintf("Element CheckSeek: %s", out)
	// case TooFast:
	// 	return fmt.Sprintf("Element TooFast: %s", out)
	// case TooSlow:
	// 	return fmt.Sprintf("Element TooSlow: %s", out)
	// case ChangeRate:
	// 	return fmt.Sprintf("Element ChangeRate: %s", out)
	// case ChangeSeek:
	// 	return fmt.Sprintf("Element ChangeSeek: %s", out)
	// case ChangeCurrent:
	// 	return fmt.Sprintf("Element ChangeCurrent: %s", out)
	// case ChangeMovies:
	// 	return fmt.Sprintf("Element ChangeMovieList: %s", out)
	// case ChangePeople:
	// 	return fmt.Sprintf("Element ChangePeopleNum: %s", out)
	// default:
	// 	return fmt.Sprintf("Element Unknown: %s", out)
	// }
	return ""
}

func (em *ElementMessage) Encode(wc io.Writer) error {
	b, err := proto.Marshal(em)
	if err != nil {
		return err
	}
	_, err = wc.Write(b)
	return err
}

type PingMessage struct{}

func (pm *PingMessage) MessageType() int {
	return websocket.PingMessage
}

func (pm *PingMessage) String() string {
	return "Ping"
}

func (pm *PingMessage) Encode(wc io.Writer) error {
	return nil
}
