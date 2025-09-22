package smtp

import (
	"bytes"
	"encoding/base64"
	"fmt"
	"mime"
	"strings"
	"time"

	smtp "github.com/emersion/go-smtp"
	"github.com/zijiren233/stream"
)

type FormatMailConfig struct {
	date                    string
	mimeVersion             string
	contentType             string
	contentTransferEncoding string
}

type FormatMailOption func(c *FormatMailConfig)

func WithDate(date time.Time) FormatMailOption {
	return func(c *FormatMailConfig) {
		c.date = date.Format(time.RFC1123Z)
	}
}

func WithMimeVersion(mimeVersion string) FormatMailOption {
	return func(c *FormatMailConfig) {
		c.mimeVersion = mimeVersion
	}
}

func WithContentType(contentType string) FormatMailOption {
	return func(c *FormatMailConfig) {
		c.contentType = contentType
	}
}

func WithContentTransferEncoding(contentTransferEncoding string) FormatMailOption {
	return func(c *FormatMailConfig) {
		c.contentTransferEncoding = contentTransferEncoding
	}
}

func FormatMail(from string, to []string, subject, body string, opts ...FormatMailOption) string {
	c := &FormatMailConfig{
		date:                    time.Now().Format(time.RFC1123Z),
		mimeVersion:             "1.0",
		contentType:             "text/html; charset=UTF-8",
		contentTransferEncoding: "base64",
	}
	for _, opt := range opts {
		opt(c)
	}

	buf := bytes.NewBuffer(nil)

	fmt.Fprintf(buf, "From: %s\r\n", from)
	fmt.Fprintf(buf, "To: %s\r\n", strings.Join(to, ", "))
	fmt.Fprintf(buf, "Subject: %s\r\n", mime.QEncoding.Encode("UTF-8", subject))
	fmt.Fprintf(buf, "Date: %s\r\n", c.date)
	fmt.Fprintf(buf, "MIME-Version: %s\r\n", c.mimeVersion)
	fmt.Fprintf(buf, "Content-Type: %s\r\n", c.contentType)

	if c.contentTransferEncoding != "" {
		fmt.Fprintf(buf, "Content-Transfer-Encoding: %s\r\n", c.contentTransferEncoding)
	}

	buf.WriteString("\r\n")

	switch c.contentTransferEncoding {
	case "base64":
		encodedBody := base64.StdEncoding.EncodeToString(stream.StringToBytes(body))
		for i := 0; i < len(encodedBody); i += 76 {
			end := i + 76
			if end > len(encodedBody) {
				end = len(encodedBody)
			}

			buf.WriteString(encodedBody[i:end] + "\r\n")
		}
	case "":
		buf.WriteString(body)
	}

	return buf.String()
}

func SendEmail(
	cli *smtp.Client,
	from string,
	to []string,
	subject, body string,
	opts ...FormatMailOption,
) error {
	return cli.SendMail(
		from,
		to,
		strings.NewReader(
			FormatMail(
				from,
				to,
				subject,
				body,
				opts...,
			),
		),
	)
}
