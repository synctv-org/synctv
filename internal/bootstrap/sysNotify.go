package bootstrap

import (
	"errors"
	"os"
	"sync"

	log "github.com/sirupsen/logrus"

	"github.com/zijiren233/gencontainer/pqueue"
	"github.com/zijiren233/gencontainer/rwmap"
)

var (
	c         chan os.Signal
	once      sync.Once
	TaskGroup rwmap.RWMap[NotifyType, *taskQueue]
	WaitCbk   func()
)

type NotifyType int

const (
	NotifyTypeEXIT NotifyType = iota + 1
	NotifyTypeRELOAD
)

type taskQueue struct {
	notifyTaskLock  sync.Mutex
	notifyTaskQueue *pqueue.PQueue[*SysNotifyTask]
}

type SysNotifyTask struct {
	Task       func() error
	NotifyType NotifyType
	Name       string
}

func NewSysNotifyTask(name string, NotifyType NotifyType, task func() error) *SysNotifyTask {
	return &SysNotifyTask{
		Name:       name,
		NotifyType: NotifyType,
		Task:       task,
	}
}

func runTask(tq *taskQueue) {
	tq.notifyTaskLock.Lock()
	defer tq.notifyTaskLock.Unlock()
	for tq.notifyTaskQueue.Len() > 0 {
		_, task := tq.notifyTaskQueue.Pop()
		func() {
			defer func() {
				if err := recover(); err != nil {
					log.Errorf("task: %s panic has returned: %v", task.Name, err)
				}
			}()
			log.Infof("task: %s running", task.Name)
			if err := task.Task(); err != nil {
				log.Errorf("task: %s an error occurred: %v", task.Name, err)
			}
			log.Infof("task: %s done", task.Name)
		}()
	}
}

func RegisterSysNotifyTask(priority int, task *SysNotifyTask) error {
	if task == nil || task.Task == nil {
		return errors.New("task is nil")
	}
	if task.NotifyType == 0 {
		panic("task notify type is 0")
	}
	tasks, _ := TaskGroup.LoadOrStore(task.NotifyType, &taskQueue{
		notifyTaskQueue: pqueue.NewMinPriorityQueue[*SysNotifyTask](),
	})
	tasks.notifyTaskLock.Lock()
	defer tasks.notifyTaskLock.Unlock()
	tasks.notifyTaskQueue.Push(priority, task)
	return nil
}
