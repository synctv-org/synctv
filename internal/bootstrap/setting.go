package bootstrap

import (
	"context"

	"github.com/synctv-org/synctv/internal/db"
	"github.com/synctv-org/synctv/internal/model"
	"github.com/synctv-org/synctv/internal/settings"
	"github.com/zijiren233/gencontainer/heap"
)

var _ heap.Interface[maxHeapItem] = (*maxHeap)(nil)

type maxHeapItem struct {
	priority int
	settings.Setting
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

func InitSetting(ctx context.Context) error {
	ss := []settings.Setting{}
	for _, s := range settings.Settings {
		ss = append(ss, s)
	}
	return initAndFixSettings(ss)
}

func settingEqual(s *model.Setting, b settings.Setting) bool {
	return s.Type == b.Type() && s.Group == b.Group() && s.Name == b.Name()
}

func initAndFixSettings(ss []settings.Setting) error {
	settingsCache, err := db.GetSettingItemsToMap()
	if err != nil {
		return err
	}
	var setting *model.Setting
	list := new(maxHeap)
	for _, s := range ss {
		heap.Push(list, maxHeapItem{
			priority: s.InitPriority(),
			Setting:  s,
		})
	}

	for list.Len() > 0 {
		b := heap.Pop(list)

		if sc, ok := settingsCache[b.Name()]; ok && settingEqual(sc, b) {
			setting = sc
		} else {
			setting = &model.Setting{
				Name:  b.Name(),
				Value: b.DefaultString(),
				Type:  b.Type(),
				Group: b.Group(),
			}
			err := db.FirstOrCreateSettingItemValue(setting)
			if err != nil {
				return err
			}
		}
		err = b.Init(setting.Value)
		if err != nil {
			// auto fix
			err = b.SetString(b.DefaultString())
			if err != nil {
				return err
			}
		}
	}

	return nil
}
