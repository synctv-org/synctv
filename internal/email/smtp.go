package email

import (
	"errors"
	"strings"
	"sync"

	"github.com/synctv-org/synctv/internal/model"
	"github.com/synctv-org/synctv/internal/settings"
	"github.com/synctv-org/synctv/utils/smtp"
)

var (
	smtpPool      *smtp.Pool
	configChanged bool
	lock          sync.Mutex
)

var (
	smtpHost = settings.NewStringSetting(
		"smtp_host",
		"",
		model.SettingGroupEmail,
		settings.WithAfterSetString(func(_ settings.StringSetting, _ string) {
			lock.Lock()
			defer lock.Unlock()
			configChanged = true
		}),
	)
	// Generally speaking, TLS uses port 587 and SSL uses port 465.
	smtpPort = settings.NewInt64Setting(
		"smtp_port",
		587,
		model.SettingGroupEmail,
		settings.WithValidatorInt64(func(i int64) error {
			if i <= 0 {
				return errors.New("smtp port must be greater than 0")
			}
			if i > 65535 {
				return errors.New("smtp port must be less than 65535")
			}
			return nil
		}),
		settings.WithAfterSetInt64(func(_ settings.Int64Setting, _ int64) {
			lock.Lock()
			defer lock.Unlock()
			configChanged = true
		}),
	)
	smtpProtocol = settings.NewStringSetting(
		"smtp_protocol",
		"TLS",
		model.SettingGroupEmail,
		settings.WithValidatorString(func(s string) error {
			s = strings.ToLower(s)
			switch s {
			case "tcp", "tls", "ssl", "":
				return nil
			default:
				return errors.New("smtp protocol must be tcp, tls or ssl")
			}
		}),
		settings.WithAfterSetString(func(_ settings.StringSetting, _ string) {
			lock.Lock()
			defer lock.Unlock()
			configChanged = true
		}),
	)
	smtpUsername = settings.NewStringSetting(
		"smtp_username",
		"",
		model.SettingGroupEmail,
		settings.WithAfterSetString(func(_ settings.StringSetting, _ string) {
			lock.Lock()
			defer lock.Unlock()
			configChanged = true
		}),
	)
	smtpPassword = settings.NewStringSetting(
		"smtp_password",
		"",
		model.SettingGroupEmail,
		settings.WithAfterSetString(func(_ settings.StringSetting, _ string) {
			lock.Lock()
			defer lock.Unlock()
			configChanged = true
		}),
	)
	smtpFrom = settings.NewStringSetting(
		"smtp_from",
		"",
		model.SettingGroupEmail,
		settings.WithAfterSetString(func(_ settings.StringSetting, s string) {
			lock.Lock()
			defer lock.Unlock()

			if smtpPool != nil {
				smtpPool.SetFrom(s)
			}
		}),
	)
	smtpPoolSize = settings.NewInt64Setting(
		"smtp_pool_size",
		10,
		model.SettingGroupEmail,
		settings.WithValidatorInt64(func(i int64) error {
			if i <= 0 {
				return errors.New("smtp pool size must be greater than 0")
			}
			if i > 100 {
				return errors.New("smtp pool size must be less than 100")
			}
			return nil
		}),
		settings.WithAfterSetInt64(func(_ settings.Int64Setting, _ int64) {
			lock.Lock()
			defer lock.Unlock()
			configChanged = true
		}),
	)
)

//nolint:gosec
func newSMTPConfig() *smtp.Config {
	return &smtp.Config{
		Host:     smtpHost.Get(),
		Port:     uint32(smtpPort.Get()),
		Protocol: smtpProtocol.Get(),
		Username: smtpUsername.Get(),
		Password: smtpPassword.Get(),
		From:     smtpFrom.Get(),
	}
}

func newSMTPPool() (*smtp.Pool, error) {
	return smtp.NewSMTPPool(newSMTPConfig(), int(smtpPoolSize.Get()))
}

func getSMTPPool() (*smtp.Pool, error) {
	lock.Lock()
	defer lock.Unlock()

	if configChanged {
		configChanged = false
		if smtpPool != nil {
			smtpPool.Close()
			smtpPool = nil
		}
	}

	if smtpPool == nil {
		pool, err := newSMTPPool()
		if err != nil {
			return nil, err
		}
		smtpPool = pool
	}

	return smtpPool, nil
}

func closeSMTPPool() {
	lock.Lock()
	defer lock.Unlock()

	if smtpPool != nil {
		smtpPool.Close()
		smtpPool = nil
	}
}
