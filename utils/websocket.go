package utils

import (
	"net/http"
	"time"

	"github.com/gorilla/websocket"
)

type WebSocket struct {
	Heartbeat time.Duration
}

func DefaultWebSocket() *WebSocket {
	return &WebSocket{Heartbeat: time.Second * 5}
}

type WebSocketConfig func(*WebSocket)

func WithHeartbeatInterval(d time.Duration) WebSocketConfig {
	return func(ws *WebSocket) {
		ws.Heartbeat = d
	}
}

func NewWebSocketServer(conf ...WebSocketConfig) *WebSocket {
	ws := DefaultWebSocket()
	for _, wsc := range conf {
		wsc(ws)
	}

	return ws
}

func (ws *WebSocket) Server(
	w http.ResponseWriter,
	r *http.Request,
	subprotocols []string,
	handler func(c *websocket.Conn) error,
) error {
	conf := []UpgraderConf{}
	if len(subprotocols) > 0 {
		conf = append(conf, WithSubprotocols(subprotocols))
	}

	wsc, err := ws.NewWebSocketClient(w, r, nil, conf...)
	if err != nil {
		return err
	}
	defer wsc.Close()

	return handler(wsc)
}

type UpgraderConf func(*websocket.Upgrader)

func WithSubprotocols(subprotocols []string) UpgraderConf {
	return func(ug *websocket.Upgrader) {
		ug.Subprotocols = subprotocols
	}
}

func (ws *WebSocket) newUpgrader(conf ...UpgraderConf) *websocket.Upgrader {
	ug := &websocket.Upgrader{
		HandshakeTimeout: time.Second * 30,
		ReadBufferSize:   1024,
		WriteBufferSize:  1024,
		CheckOrigin: func(_ *http.Request) bool {
			return true
		},
	}
	for _, uc := range conf {
		uc(ug)
	}

	return ug
}

func (ws *WebSocket) NewWebSocketClient(
	w http.ResponseWriter,
	r *http.Request,
	responseHeader http.Header,
	conf ...UpgraderConf,
) (*websocket.Conn, error) {
	conn, err := ws.newUpgrader(conf...).Upgrade(w, r, responseHeader)
	if err != nil {
		return nil, err
	}

	return conn, nil
}
