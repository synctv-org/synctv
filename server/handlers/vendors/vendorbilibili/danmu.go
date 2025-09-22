package vendorbilibili

import (
	"bytes"
	"compress/zlib"
	"context"
	"encoding/binary"
	"errors"
	"fmt"
	"io"
	"net"
	"net/http"
	"strconv"
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
	CmdHeartbeat      command = 2
	CmdHeartbeatReply command = 3
	CmdNormal         command = 5
	CmdAuth           command = 7
	CmdAuthReply      command = 8
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

//nolint:gosec
func newHeader(size uint32, command command, sequence uint32) header {
	h := header{
		TotalSize: uint32(headerLen) + size,
		HeaderLen: uint16(headerLen),
		Command:   command,
		Sequence:  sequence,
	}
	switch command {
	case CmdHeartbeat, CmdAuth:
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

//nolint:gosec
func writeVerifyHello(conn *websocket.Conn, hello *verifyHello) error {
	msg, err := json.Marshal(hello)
	if err != nil {
		return err
	}

	header := newHeader(uint32(len(msg)), CmdAuth, 1)

	headerBytes, err := header.Marshal()
	if err != nil {
		return err
	}

	return conn.WriteMessage(websocket.BinaryMessage, append(headerBytes, msg...))
}

func writeHeartbeat(conn *websocket.Conn, sequence uint32) error {
	header := newHeader(0, CmdHeartbeat, sequence)

	headerBytes, err := header.Marshal()
	if err != nil {
		return err
	}

	return conn.WriteMessage(websocket.BinaryMessage, headerBytes)
}

type replyCmd struct {
	Cmd string `json:"cmd"`
}

func (v *BilibiliVendorService) StreamDanmu(
	ctx context.Context,
	handler func(danmu string) error,
) error {
	resp, err := vendor.LoadBilibiliClient("").GetLiveDanmuInfo(ctx, &bilibili.GetLiveDanmuInfoReq{
		RoomID: v.movie.VendorInfo.Bilibili.Cid,
	})
	if err != nil {
		return err
	}

	if len(resp.GetHostList()) == 0 {
		return errors.New("no host list")
	}

	wssHost := resp.GetHostList()[0].GetHost()
	wssPort := resp.GetHostList()[0].GetWssPort()

	conn, wsresp, err := websocket.
		DefaultDialer.
		DialContext(
			ctx,
			fmt.Sprintf("wss://%s/sub", net.JoinHostPort(wssHost, strconv.Itoa(int(wssPort)))),
			http.Header{
				"User-Agent": []string{utils.UA},
				"Origin":     []string{"https://live.bilibili.com"},
			},
		)
	if err != nil {
		return err
	}
	defer conn.Close()
	defer wsresp.Body.Close()

	err = writeVerifyHello(
		conn,
		newVerifyHello(
			v.movie.VendorInfo.Bilibili.Cid,
			resp.GetToken(),
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
			case CmdHeartbeatReply:
				continue
			default:
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

				_ = handler(content)
			case "DM_INTERACTION":
			}
		}
	}
}

type danmuMsg struct {
	Info []any `json:"info"`
}
