package settings

import (
	"fmt"
	"strconv"
	"sync/atomic"

	"github.com/synctv-org/synctv/internal/db"
	"github.com/synctv-org/synctv/internal/model"
)

type Int64Setting interface {
	Setting
	Set(int64) error
	Get() int64
	Default() int64
	Parse(string) (int64, error)
	Stringify(int64) string
}

var _ Int64Setting = (*Int64)(nil)

type Int64 struct {
	setting
	defaultValue int64
	value        int64
}

func NewInt64(name string, value int64, group model.SettingGroup) *Int64 {
	i := &Int64{
		setting: setting{
			name:  name,
			group: group,
		},
		defaultValue: value,
		value:        value,
	}
	return i
}

func (i *Int64) Parse(value string) (int64, error) {
	return strconv.ParseInt(value, 10, 64)
}

func (i *Int64) Stringify(value int64) string {
	return strconv.FormatInt(value, 10)
}

func (i *Int64) Init(value string) error {
	v, err := i.Parse(value)
	if err != nil {
		return err
	}
	i.set(v)
	return nil
}

func (i *Int64) Default() int64 {
	return i.defaultValue
}

func (i *Int64) DefaultRaw() string {
	return strconv.FormatInt(i.defaultValue, 10)
}

func (i *Int64) DefaultInterface() any {
	return i.defaultValue
}

func (i *Int64) Raw() string {
	return i.Stringify(i.Get())
}

func (i *Int64) SetRaw(value string) error {
	err := i.Init(value)
	if err != nil {
		return err
	}
	return db.UpdateSettingItemValue(i.Name(), i.Raw())
}

func (i *Int64) set(value int64) {
	atomic.StoreInt64(&i.value, value)
}

func (i *Int64) Set(value int64) error {
	err := db.UpdateSettingItemValue(i.Name(), i.Stringify(value))
	if err != nil {
		return err
	}
	i.set(value)
	return nil
}

func (i *Int64) Get() int64 {
	return atomic.LoadInt64(&i.value)
}

func (i *Int64) Interface() any {
	return i.Get()
}

func newInt64Setting(k string, v int64, g model.SettingGroup) *Int64 {
	_, loaded := Settings[k]
	if loaded {
		panic(fmt.Sprintf("setting %s already exists", k))
	}
	i := NewInt64(k, v, g)
	Settings[k] = i
	GroupSettings[g] = append(GroupSettings[g], i)
	return i
}
