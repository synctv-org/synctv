package settings

import (
	"fmt"
	"strconv"
	"sync/atomic"

	"github.com/synctv-org/synctv/internal/db"
	"github.com/synctv-org/synctv/internal/model"
)

type BoolSetting interface {
	Setting
	Set(bool) error
	Get() bool
	Default() bool
	Parse(string) (bool, error)
	Stringify(bool) string
	SetBeforeInit(func(BoolSetting, bool) (bool, error))
	SetBeforeSet(func(BoolSetting, bool) (bool, error))
	SetAfterGet(func(BoolSetting, bool) bool)
}

var _ BoolSetting = (*Bool)(nil)

type Bool struct {
	beforeInit func(BoolSetting, bool) (bool, error)
	beforeSet  func(BoolSetting, bool) (bool, error)
	afterInit  func(BoolSetting, bool)
	afterSet   func(BoolSetting, bool)
	afterGet   func(BoolSetting, bool) bool
	setting
	value        atomic.Bool
	defaultValue bool
}

type BoolSettingOption func(*Bool)

func WithInitPriorityBool(priority int) BoolSettingOption {
	return func(s *Bool) {
		s.SetInitPriority(priority)
	}
}

func WithBeforeInitBool(beforeInit func(BoolSetting, bool) (bool, error)) BoolSettingOption {
	return func(s *Bool) {
		s.SetBeforeInit(beforeInit)
	}
}

func WithBeforeSetBool(beforeSet func(BoolSetting, bool) (bool, error)) BoolSettingOption {
	return func(s *Bool) {
		s.SetBeforeSet(beforeSet)
	}
}

func WithAfterInitBool(afterInit func(BoolSetting, bool)) BoolSettingOption {
	return func(s *Bool) {
		s.SetAfterInit(afterInit)
	}
}

func WithAfterSetBool(afterSet func(BoolSetting, bool)) BoolSettingOption {
	return func(s *Bool) {
		s.SetAfterSet(afterSet)
	}
}

func WithAfterGetBool(afterGet func(BoolSetting, bool) bool) BoolSettingOption {
	return func(s *Bool) {
		s.SetAfterGet(afterGet)
	}
}

func newBool(name string, value bool, group model.SettingGroup, options ...BoolSettingOption) *Bool {
	b := &Bool{
		setting: setting{
			name:        name,
			group:       group,
			settingType: model.SettingTypeBool,
		},
		defaultValue: value,
	}
	for _, option := range options {
		option(b)
	}
	b.set(value)
	return b
}

func (b *Bool) SetBeforeInit(beforeInit func(BoolSetting, bool) (bool, error)) {
	b.beforeInit = beforeInit
}

func (b *Bool) SetBeforeSet(beforeSet func(BoolSetting, bool) (bool, error)) {
	b.beforeSet = beforeSet
}

func (b *Bool) SetAfterInit(afterInit func(BoolSetting, bool)) {
	b.afterInit = afterInit
}

func (b *Bool) SetAfterSet(afterSet func(BoolSetting, bool)) {
	b.afterSet = afterSet
}

func (b *Bool) SetAfterGet(afterGet func(BoolSetting, bool) bool) {
	b.afterGet = afterGet
}

func (b *Bool) set(value bool) {
	b.value.Store(value)
}

func (b *Bool) Get() bool {
	v := b.value.Load()
	if b.afterGet != nil {
		v = b.afterGet(b, v)
	}
	return v
}

func (b *Bool) Init(value string) error {
	if b.Inited() {
		return ErrSettingAlreadyInited
	}

	v, err := b.Parse(value)
	if err != nil {
		return err
	}

	if b.beforeInit != nil {
		v, err = b.beforeInit(b, v)
		if err != nil {
			return err
		}
	}

	b.set(v)

	if b.afterInit != nil {
		b.afterInit(b, v)
	}

	b.inited = true

	return nil
}

func (b *Bool) Parse(value string) (bool, error) {
	return strconv.ParseBool(value)
}

func (b *Bool) Stringify(value bool) string {
	return strconv.FormatBool(value)
}

func (b *Bool) Default() bool {
	return b.defaultValue
}

func (b *Bool) DefaultString() string {
	return b.Stringify(b.defaultValue)
}

func (b *Bool) String() string {
	return b.Stringify(b.Get())
}

func (b *Bool) DefaultInterface() any {
	return b.Default()
}

func (b *Bool) SetString(value string) error {
	v, err := b.Parse(value)
	if err != nil {
		return err
	}

	if b.beforeSet != nil {
		v, err = b.beforeSet(b, v)
		if err != nil {
			return err
		}
	}

	err = db.UpdateSettingItemValue(b.name, b.Stringify(v))
	if err != nil {
		return err
	}

	b.set(v)

	if b.afterSet != nil {
		b.afterSet(b, v)
	}

	return nil
}

func (b *Bool) Set(v bool) (err error) {
	if b.beforeSet != nil {
		v, err = b.beforeSet(b, v)
		if err != nil {
			return err
		}
	}

	err = db.UpdateSettingItemValue(b.name, b.Stringify(v))
	if err != nil {
		return err
	}

	b.set(v)

	if b.afterSet != nil {
		b.afterSet(b, v)
	}

	return
}

func (b *Bool) Interface() any {
	return b.Get()
}

func NewBoolSetting(k string, v bool, g model.SettingGroup, options ...BoolSettingOption) BoolSetting {
	_, loaded := Settings[k]
	if loaded {
		panic(fmt.Sprintf("setting %s already exists", k))
	}
	return CoverBoolSetting(k, v, g, options...)
}

func CoverBoolSetting(k string, v bool, g model.SettingGroup, options ...BoolSettingOption) BoolSetting {
	b := newBool(k, v, g, options...)
	Settings[k] = b
	if GroupSettings[g] == nil {
		GroupSettings[g] = make(map[model.SettingGroup]Setting)
	}
	GroupSettings[g][k] = b
	pushNeedInit(b)
	return b
}

func LoadBoolSetting(k string) (BoolSetting, bool) {
	s, ok := Settings[k]
	if !ok {
		return nil, false
	}
	b, ok := s.(BoolSetting)
	return b, ok
}

func LoadOrNewBoolSetting(k string, v bool, g model.SettingGroup, options ...BoolSettingOption) BoolSetting {
	if s, ok := LoadBoolSetting(k); ok {
		return s
	}
	return CoverBoolSetting(k, v, g, options...)
}
