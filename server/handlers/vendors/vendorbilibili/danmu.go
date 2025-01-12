package vendorbilibili

import (
	"bytes"
	"compress/zlib"
	"context"
	"encoding/binary"
	"errors"
	"fmt"
	"io"
	"net/http"
	"time"

	"github.com/andybalholm/brotli"
	"github.com/gorilla/websocket"
	json "github.com/json-iterator/go"
	log "github.com/sirupsen/logrus"
	"github.com/synctv-org/synctv/internal/vendor"
	"github.com/synctv-org/synctv/utils"
	"github.com/synctv-org/vendors/api/bilibili"
)

type command uint32

const (
	CMD_HEARTBEAT       command = 2
	CMD_HEARTBEAT_REPLY command = 3
	CMD_NORMAL          command = 5
	CMD_AUTH            command = 7
	CMD_AUTH_REPLY      command = 8
)

type header struct {
	TotalSize uint32
	HeaderLen uint16
	Version   uint16
	Command   command
	Sequence  uint32
}

var headerLen = binary.Size(header{})

func (h *header) Marshal() ([]byte, error) {
	buf := bytes.NewBuffer(make([]byte, 0, headerLen))
	err := binary.Write(buf, binary.BigEndian, h)
	if err != nil {
		return nil, err
	}
	return buf.Bytes(), nil
}

func (h *header) Unmarshal(data []byte) error {
	return binary.Read(bytes.NewReader(data), binary.BigEndian, h)
}

func newHeader(size uint32, command command, sequence uint32) header {
	h := header{
		TotalSize: uint32(headerLen) + size,
		HeaderLen: uint16(headerLen),
		Command:   command,
		Sequence:  sequence,
	}
	switch command {
	case CMD_HEARTBEAT, CMD_AUTH:
		h.Version = 1
	}
	return h
}

type verifyHello struct {
	UID      int64  `json:"uid"`
	RoomID   uint64 `json:"roomid,omitempty"`
	ProtoVer int    `json:"protover,omitempty"`
	Platform string `json:"platform,omitempty"`
	Type     int    `json:"type,omitempty"`
	Key      string `json:"key,omitempty"`
}

func newVerifyHello(roomID uint64, key string) *verifyHello {
	return &verifyHello{
		RoomID:   roomID,
		ProtoVer: 3,
		Platform: "web",
		Type:     2,
		Key:      key,
	}
}

func writeVerifyHello(conn *websocket.Conn, hello *verifyHello) error {
	msg, err := json.Marshal(hello)
	if err != nil {
		return err
	}
	header := newHeader(uint32(len(msg)), CMD_AUTH, 1)
	headerBytes, err := header.Marshal()
	if err != nil {
		return err
	}
	return conn.WriteMessage(websocket.BinaryMessage, append(headerBytes, msg...))
}

func writeHeartbeat(conn *websocket.Conn, sequence uint32) error {
	header := newHeader(0, CMD_HEARTBEAT, sequence)
	headerBytes, err := header.Marshal()
	if err != nil {
		return err
	}
	return conn.WriteMessage(websocket.BinaryMessage, headerBytes)
}

type replyCmd struct {
	Cmd string `json:"cmd"`
}

func (v *BilibiliVendorService) StreamDanmu(ctx context.Context, handler func(danmu string) error) error {
	resp, err := vendor.LoadBilibiliClient("").GetLiveDanmuInfo(ctx, &bilibili.GetLiveDanmuInfoReq{
		RoomID: v.movie.VendorInfo.Bilibili.Cid,
	})
	if err != nil {
		return err
	}
	if len(resp.HostList) == 0 {
		return errors.New("no host list")
	}
	wssHost := resp.HostList[0].Host
	wssPort := resp.HostList[0].WssPort

	conn, _, err := websocket.
		DefaultDialer.
		DialContext(
			ctx,
			fmt.Sprintf("wss://%s:%d/sub", wssHost, wssPort),
			http.Header{
				"User-Agent": []string{utils.UA},
				"Origin":     []string{"https://live.bilibili.com"},
			},
		)
	if err != nil {
		return err
	}
	defer conn.Close()

	err = writeVerifyHello(
		conn,
		newVerifyHello(
			v.movie.VendorInfo.Bilibili.Cid,
			resp.Token,
		),
	)
	if err != nil {
		return err
	}

	_, _, err = conn.ReadMessage()
	if err != nil {
		return err
	}

	go func() {
		ticker := time.NewTicker(time.Second * 20)
		defer ticker.Stop()
		sequence := uint32(1)
		for {
			select {
			case <-ctx.Done():
				return
			case <-ticker.C:
				sequence++
				err = writeHeartbeat(conn, sequence)
				if err != nil {
					log.Errorf("write heartbeat error: %v", err)
				}
			}
		}
	}()

	for {
		select {
		case <-ctx.Done():
			return ctx.Err()
		default:
			_, message, err := conn.ReadMessage()
			if err != nil {
				return err
			}
			header := header{}
			err = header.Unmarshal(message[:headerLen])
			if err != nil {
				return err
			}
			switch header.Command {
			case CMD_HEARTBEAT_REPLY:
				continue
			}
			data := message[headerLen:]
			switch header.Version {
			case 2:
				// zlib
				zlibReader, err := zlib.NewReader(bytes.NewReader(data))
				if err != nil {
					return err
				}
				defer zlibReader.Close()
				data, err = io.ReadAll(zlibReader)
				if err != nil {
					return err
				}
			case 3:
				// brotli
				brotliReader := brotli.NewReader(bytes.NewReader(data))
				data, err = io.ReadAll(brotliReader)
				if err != nil {
					return err
				}
				data = data[headerLen:]
			}
			reply := replyCmd{}
			err = json.Unmarshal(data, &reply)
			if err != nil {
				return err
			}
			switch reply.Cmd {
			case "DANMU_MSG":
				danmu := danmuMsg{}
				err = json.Unmarshal(data, &danmu)
				if err != nil {
					return err
				}
				content, ok := danmu.Info[1].(string)
				if !ok {
					return errors.New("content is not string")
				}
				handler(content)
			case "DM_INTERACTION":
			}
		}
	}
}

type danmuMsg struct {
	Info []any `json:"info"`
}
