package smtp

import (
	"encoding/base64"
	"fmt"
	"strings"

	smtp "github.com/emersion/go-smtp"
	"github.com/zijiren233/stream"
)

func FormatMail(from string, to []string, subject string, body any) string {
	return fmt.Sprintf(
		"From: %s\r\nTo: %s\r\nSubject: =?UTF-8?B?%s?=\r\nContent-Type: text/html; charset=UTF-8\r\n\r\n%v",
		from,
		strings.Join(to, ","),
		base64.StdEncoding.EncodeToString(stream.StringToBytes(subject)),
		body,
	)
}

func SendEmail(cli *smtp.Client, from string, to []string, subject, body string) error {
	return cli.SendMail(
		from,
		to,
		strings.NewReader(
			FormatMail(
				from,
				to,
				subject,
				body,
			),
		),
	)
}
