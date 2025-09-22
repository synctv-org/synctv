package settings

import (
	"errors"
	"fmt"

	json "github.com/json-iterator/go"
	"github.com/synctv-org/synctv/internal/model"
	"github.com/zijiren233/gencontainer/heap"
)

var ErrSettingAlreadyInited = errors.New("setting already inited")

var _ heap.Interface[maxHeapItem] = (*maxHeap)(nil)

type maxHeapItem struct {
	Setting
	priority int
}

type maxHeap struct {
	items []maxHeapItem
}

func (h *maxHeap) Len() int {
	return len(h.items)
}

func (h *maxHeap) Less(i, j int) bool {
	return h.items[i].priority > h.items[j].priority
}

func (h *maxHeap) Swap(i, j int) {
	h.items[i], h.items[j] = h.items[j], h.items[i]
}

func (h *maxHeap) Push(x maxHeapItem) {
	h.items = append(h.items, x)
}

func (h *maxHeap) Pop() maxHeapItem {
	n := len(h.items)
	x := h.items[n-1]
	h.items = h.items[:n-1]
	return x
}

var (
	Settings      = make(map[string]Setting)
	GroupSettings = make(map[model.SettingGroup]map[string]Setting)
	needInit      = new(maxHeap)
)

func pushNeedInit(s Setting) {
	if s == nil {
		panic("push need init failed, setting is nil")
	}

	for i, item := range needInit.items {
		if item.Name() == s.Name() {
			heap.Remove(needInit, i)
			break
		}
	}

	heap.Push(needInit, maxHeapItem{
		priority: s.InitPriority(),
		Setting:  s,
	})
}

func hasNeedInit() bool {
	return needInit.Len() > 0
}

func PopNeedInit() (Setting, bool) {
	for hasNeedInit() {
		item := heap.Pop(needInit)

		s := item.Setting
		if s.Inited() {
			continue
		}

		return s, true
	}

	return nil, false
}

type Setting interface {
	Name() string
	Type() model.SettingType
	Group() model.SettingGroup
	Init(value string) error
	Inited() bool
	SetInitPriority(priority int)
	InitPriority() int
	String() string
	SetString(value string) error
	DefaultString() string
	DefaultInterface() any
	Interface() any
}

//nolint:errcheck
func SetValue(name string, value any) error {
	s, ok := Settings[name]
	if !ok {
		return fmt.Errorf("setting %s not found", name)
	}

	switch s.Type() {
	case model.SettingTypeBool:
		return s.(BoolSetting).Set(json.Wrap(value).ToBool())
	case model.SettingTypeInt64:
		return s.(Int64Setting).Set(json.Wrap(value).ToInt64())
	case model.SettingTypeFloat64:
		return s.(Float64Setting).Set(json.Wrap(value).ToFloat64())
	case model.SettingTypeString:
		return s.(StringSetting).Set(json.Wrap(value).ToString())
	}

	return s.SetString(json.Wrap(value).ToString())
}

type setting struct {
	name         string
	settingType  model.SettingType
	group        model.SettingGroup
	initPriority int
	inited       bool
}

func (d *setting) Name() string {
	return d.name
}

func (d *setting) Type() model.SettingType {
	return d.settingType
}

func (d *setting) Group() model.SettingGroup {
	return d.group
}

func (d *setting) InitPriority() int {
	return d.initPriority
}

func (d *setting) Inited() bool {
	return d.inited
}

func (d *setting) SetInitPriority(priority int) {
	d.initPriority = priority
}
