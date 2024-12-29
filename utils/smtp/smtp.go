package smtp

import (
	"crypto/tls"
	"errors"
	"fmt"
	"gopkg.in/gomail.v2"
	"strings"
)

type Config struct {
	Host     string
	Protocol string
	Username string
	Password string
	From     string
	Port     uint32
}

func validateSMTPConfig(c *Config) error {
	if c == nil {
		return errors.New("smtp config is nil")
	}
	if c.Host == "" {
		return errors.New("smtp host is empty")
	}
	if c.Port == 0 {
		return errors.New("smtp port is empty")
	}
	if c.Username == "" {
		return errors.New("smtp username is empty")
	}
	if c.Password == "" {
		return errors.New("smtp password is empty")
	}
	if c.From == "" {
		return errors.New("smtp from is empty")
	}
	return nil
}

type Mailer struct {
	config *Config
}

func NewMailer(c *Config) (*Mailer, error) {
	if err := validateSMTPConfig(c); err != nil {
		return nil, err
	}
	return &Mailer{config: c}, nil
}

func newDialer(c *Config) *gomail.Dialer {
	d := gomail.NewDialer(c.Host, int(c.Port), c.Username, c.Password)
	switch strings.ToUpper(c.Protocol) {
	case "TLS": // 587
		d.TLSConfig = &tls.Config{
			ServerName: c.Host,
		}
	case "SSL": // 465
		d.SSL = true
		d.TLSConfig = &tls.Config{
			ServerName: c.Host,
		}
	case "TCP": // PlainText
		d.SSL = false
		d.TLSConfig = nil
	default:
		d.TLSConfig = &tls.Config{
			ServerName: c.Host,
		}
	}
	return d
}

func (m *Mailer) SendEmail(to []string, subject, body string, opts ...func(*gomail.Message)) error {
	msg := gomail.NewMessage()
	msg.SetHeader("From", m.config.From)
	msg.SetHeader("To", to...)
	msg.SetHeader("Subject", subject)
	msg.SetBody("text/html", body)

	for _, opt := range opts {
		if opt != nil {
			opt(msg)
		}
	}

	dialer := newDialer(m.config)
	if err := dialer.DialAndSend(msg); err != nil {
		return fmt.Errorf("failed to send email: %w", err)
	}
	return nil
}

func (m *Mailer) SetFrom(from string) {
	m.config.From = from
}
