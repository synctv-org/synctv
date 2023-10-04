package room

import (
	"encoding/json"
	"fmt"
	"io"

	"github.com/gorilla/websocket"
	"gopkg.in/yaml.v3"
)

type Message interface {
	MessageType() int
	String() string
	Encode(wc io.Writer) error
}

type ElementMessageType int

const (
	Error ElementMessageType = iota + 1
	ChatMessage
	Play
	Pause
	CheckSeek
	TooFast
	TooSlow
	ChangeRate
	ChangeSeek
	ChangeCurrent
	ChangeMovieList
	ChangePeopleNum
)

type ElementMessage struct {
	Type      ElementMessageType `json:"type" yaml:"type"`
	Sender    string             `json:"sender,omitempty" yaml:"sender,omitempty"`
	Message   string             `json:"message,omitempty" yaml:"message,omitempty"`
	Seek      float64            `json:"seek,omitempty" yaml:"seek,omitempty"`
	Rate      float64            `json:"rate,omitempty" yaml:"rate,omitempty"`
	Current   *Current           `json:"current,omitempty" yaml:"current,omitempty"`
	PeopleNum int64              `json:"peopleNum,omitempty" yaml:"peopleNum,omitempty"`
	Time      int64              `json:"time,omitempty" yaml:"time,omitempty"`
}

func (em *ElementMessage) MessageType() int {
	return websocket.TextMessage
}

func (em *ElementMessage) String() string {
	out, _ := yaml.Marshal(em)
	switch em.Type {
	case Error:
		return fmt.Sprintf("Element Error: %s", out)
	case ChatMessage:
		return fmt.Sprintf("Element ChatMessage: %s", out)
	case Play:
		return fmt.Sprintf("Element Play: %s", out)
	case Pause:
		return fmt.Sprintf("Element Pause: %s", out)
	case CheckSeek:
		return fmt.Sprintf("Element CheckSeek: %s", out)
	case TooFast:
		return fmt.Sprintf("Element TooFast: %s", out)
	case TooSlow:
		return fmt.Sprintf("Element TooSlow: %s", out)
	case ChangeRate:
		return fmt.Sprintf("Element ChangeRate: %s", out)
	case ChangeSeek:
		return fmt.Sprintf("Element ChangeSeek: %s", out)
	case ChangeCurrent:
		return fmt.Sprintf("Element ChangeCurrent: %s", out)
	case ChangeMovieList:
		return fmt.Sprintf("Element ChangeMovieList: %s", out)
	case ChangePeopleNum:
		return fmt.Sprintf("Element ChangePeopleNum: %s", out)
	default:
		return fmt.Sprintf("Element Unknown: %s", out)
	}
}

func (em *ElementMessage) Encode(wc io.Writer) error {
	return json.NewEncoder(wc).Encode(em)
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
