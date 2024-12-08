package smtp

import (
	"crypto/tls"
	"errors"
	"fmt"
	"gopkg.in/gomail.v2"
	"runtime"
	"strings"
	"sync"
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

var ErrSMTPPoolClosed = errors.New("smtp pool is closed")

type Pool struct {
	c       *Config
	senders []*gomail.Dialer
	poolCap int
	active  int
	mu      sync.Mutex
	closed  bool
}

func NewSMTPPool(c *Config, poolCap int) (*Pool, error) {
	err := validateSMTPConfig(c)
	if err != nil {
		return nil, err
	}
	return &Pool{
		senders: make([]*gomail.Dialer, 0, poolCap),
		c:       c,
		poolCap: poolCap,
	}, nil
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

func (p *Pool) Get() (*gomail.Dialer, error) {
	p.mu.Lock()
	if p.closed {
		p.mu.Unlock()
		return nil, ErrSMTPPoolClosed
	}
	if len(p.senders) > 0 {
		dialer := p.senders[len(p.senders)-1]
		p.senders = p.senders[:len(p.senders)-1]
		p.active++
		p.mu.Unlock()
		return dialer, nil
	}
	if p.active >= p.poolCap {
		p.mu.Unlock()
		runtime.Gosched()
		return p.Get()
	}
	dialer := newDialer(p.c)
	p.active++
	p.mu.Unlock()
	return dialer, nil
}

func (p *Pool) Put(dialer *gomail.Dialer) {
	if dialer == nil {
		return
	}
	p.mu.Lock()
	defer p.mu.Unlock()
	p.active--
	if p.closed {
		return
	}
	p.senders = append(p.senders, dialer)
}

func (p *Pool) Close() {
	p.mu.Lock()
	defer p.mu.Unlock()
	p.closed = true
	p.senders = nil
}

func (p *Pool) SendEmail(to []string, subject, body string, opts ...func(*gomail.Message)) error {
	dialer, err := p.Get()
	if err != nil {
		return err
	}
	defer p.Put(dialer)

	m := gomail.NewMessage()
	m.SetHeader("From", p.c.From)
	m.SetHeader("To", to...)
	m.SetHeader("Subject", subject)
	m.SetBody("text/html", body)

	for _, opt := range opts {
		if opt != nil {
			opt(m)
		}
	}

	if err := dialer.DialAndSend(m); err != nil {
		return fmt.Errorf("failed to send email: %w", err)
	}
	return nil
}

func (p *Pool) SetFrom(from string) {
	p.mu.Lock()
	defer p.mu.Unlock()
	p.c.From = from
}
