package smtp

import (
	"errors"
	"fmt"
	"runtime"
	"strings"
	"sync"

	"github.com/emersion/go-sasl"
	smtp "github.com/emersion/go-smtp"
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

func newSMTPClient(c *Config) (*smtp.Client, error) {
	var (
		cli *smtp.Client
		err error
	)

	switch strings.ToUpper(c.Protocol) {
	case "TLS": // 587
		cli, err = smtp.DialStartTLS(fmt.Sprintf("%s:%d", c.Host, c.Port), nil)
	case "SSL": // 465
		cli, err = smtp.DialTLS(fmt.Sprintf("%s:%d", c.Host, c.Port), nil)
	default:
		cli, err = smtp.Dial(fmt.Sprintf("%s:%d", c.Host, c.Port))
	}

	if err != nil {
		return nil, fmt.Errorf("dial smtp server failed: %w", err)
	}

	err = cli.Auth(sasl.NewLoginClient(c.Username, c.Password))
	if err != nil {
		cli.Close()
		return nil, fmt.Errorf("auth failed: %w", err)
	}

	return cli, nil
}

var ErrSMTPPoolClosed = errors.New("smtp pool is closed")

type Pool struct {
	c       *Config
	clients []*smtp.Client
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
		clients: make([]*smtp.Client, 0, poolCap),
		c:       c,
		poolCap: poolCap,
	}, nil
}

func (p *Pool) Get() (*smtp.Client, error) {
	p.mu.Lock()

	if p.closed {
		p.mu.Unlock()
		return nil, ErrSMTPPoolClosed
	}

	if len(p.clients) > 0 {
		cli := p.clients[len(p.clients)-1]
		p.clients = p.clients[:len(p.clients)-1]
		p.active++
		p.mu.Unlock()

		if cli.Noop() != nil {
			cli.Close()
			p.mu.Lock()
			p.active--
			p.mu.Unlock()

			return p.Get()
		}

		return cli, nil
	}

	if p.active >= p.poolCap {
		p.mu.Unlock()
		runtime.Gosched()
		return p.Get()
	}

	cli, err := newSMTPClient(p.c)
	if err != nil {
		p.mu.Unlock()
		return nil, err
	}

	p.active++
	p.mu.Unlock()

	return cli, nil
}

func (p *Pool) Put(cli *smtp.Client) {
	if cli == nil {
		return
	}

	noopErr := cli.Noop()

	p.mu.Lock()
	defer p.mu.Unlock()

	p.active--

	if p.closed || noopErr != nil {
		cli.Close()
		return
	}

	p.clients = append(p.clients, cli)
}

func (p *Pool) Close() {
	p.mu.Lock()
	defer p.mu.Unlock()

	p.closed = true

	for _, cli := range p.clients {
		cli.Close()
	}

	p.clients = nil
}

func (p *Pool) SendEmail(to []string, subject, body string, opts ...FormatMailOption) error {
	cli, err := p.Get()
	if err != nil {
		return err
	}
	defer p.Put(cli)

	return SendEmail(cli, p.c.From, to, subject, body, opts...)
}

func (p *Pool) SetFrom(from string) {
	p.mu.Lock()
	defer p.mu.Unlock()

	p.c.From = from
}
