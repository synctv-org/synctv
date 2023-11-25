package settings

import (
	"fmt"
	"math"
	"strconv"
	"sync/atomic"

	"github.com/synctv-org/synctv/internal/db"
	"github.com/synctv-org/synctv/internal/model"
)

type Float64Setting interface {
	Setting
	Set(float64) error
	Get() float64
	Default() float64
	Parse(string) (float64, error)
	Stringify(float64) string
}

var _ Float64Setting = (*Float64)(nil)

type Float64 struct {
	setting
	defaultValue          float64
	value                 uint64
	validator             func(float64) error
	beforeInit, beforeSet func(Float64Setting, float64) error
}

type Float64SettingOption func(*Float64)

func WithValidatorFloat64(validator func(float64) error) Float64SettingOption {
	return func(s *Float64) {
		s.validator = validator
	}
}

func WithBeforeInitFloat64(beforeInit func(Float64Setting, float64) error) Float64SettingOption {
	return func(s *Float64) {
		s.beforeInit = beforeInit
	}
}

func WithBeforeSetFloat64(beforeSet func(Float64Setting, float64) error) Float64SettingOption {
	return func(s *Float64) {
		s.beforeSet = beforeSet
	}
}

func newFloat64(name string, value float64, group model.SettingGroup, options ...Float64SettingOption) *Float64 {
	f := &Float64{
		setting: setting{
			name:        name,
			group:       group,
			settingType: model.SettingTypeFloat64,
		},
		defaultValue: value,
	}
	for _, option := range options {
		option(f)
	}
	f.set(value)
	return f
}

func (f *Float64) Parse(value string) (float64, error) {
	v, err := strconv.ParseFloat(value, 64)
	if err != nil {
		return 0, err
	}
	if f.validator != nil {
		return v, f.validator(v)
	}
	return v, nil
}

func (f *Float64) Stringify(value float64) string {
	return strconv.FormatFloat(value, 'f', -1, 64)
}

func (f *Float64) Init(value string) error {
	v, err := f.Parse(value)
	if err != nil {
		return err
	}

	if f.beforeInit != nil {
		err = f.beforeInit(f, v)
		if err != nil {
			return err
		}
	}

	f.set(v)
	return nil
}

func (f *Float64) String() string {
	return f.Stringify(f.Get())
}

func (f *Float64) Default() float64 {
	return f.defaultValue
}

func (f *Float64) DefaultString() string {
	return f.Stringify(f.defaultValue)
}

func (f *Float64) DefaultInterface() any {
	return f.Default()
}

func (f *Float64) SetString(value string) error {
	v, err := f.Parse(value)
	if err != nil {
		return err
	}

	if f.beforeSet != nil {
		err = f.beforeSet(f, v)
		if err != nil {
			return err
		}
	}

	err = db.UpdateSettingItemValue(f.name, f.Stringify(v))
	if err != nil {
		return err
	}

	f.set(v)
	return nil
}

func (f *Float64) set(value float64) {
	atomic.StoreUint64(&f.value, math.Float64bits(value))
}

func (f *Float64) Set(value float64) (err error) {
	if f.validator != nil {
		err = f.validator(value)
		if err != nil {
			return err
		}
	}

	if f.beforeSet != nil {
		err = f.beforeSet(f, value)
		if err != nil {
			return err
		}
	}

	err = db.UpdateSettingItemValue(f.name, f.Stringify(value))
	if err != nil {
		return err
	}

	f.set(value)
	return
}

func (f *Float64) Get() float64 {
	return math.Float64frombits(atomic.LoadUint64(&f.value))
}

func (f *Float64) Interface() any {
	return f.Get()
}

func NewFloat64Setting(k string, v float64, g model.SettingGroup, options ...Float64SettingOption) *Float64 {
	_, loaded := Settings[k]
	if loaded {
		panic(fmt.Sprintf("setting %s already exists", k))
	}
	f := newFloat64(k, v, g, options...)
	Settings[k] = f
	GroupSettings[g] = append(GroupSettings[g], f)
	return f
}
