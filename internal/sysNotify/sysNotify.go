package sysnotify

import (
	"errors"
	"os"
	"sync"

	log "github.com/sirupsen/logrus"

	"github.com/zijiren233/gencontainer/pqueue"
	"github.com/zijiren233/gencontainer/rwmap"
)

var sysNotify SysNotify

func Init() {
	sysNotify.Init()
}

func RegisterSysNotifyTask(priority int, task *sysNotifyTask) error {
	return sysNotify.RegisterSysNotifyTask(priority, task)
}

func WaitCbk() {
	sysNotify.WaitCbk()
}

type SysNotify struct {
	c         chan os.Signal
	taskGroup rwmap.RWMap[NotifyType, *taskQueue]
	once      sync.Once
}

type NotifyType int

const (
	NotifyTypeEXIT NotifyType = iota + 1
	NotifyTypeRELOAD
)

type taskQueue struct {
	notifyTaskQueue *pqueue.PQueue[*sysNotifyTask]
	notifyTaskLock  sync.Mutex
}

type sysNotifyTask struct {
	Task       func() error
	Name       string
	NotifyType NotifyType
}

func NewSysNotifyTask(name string, NotifyType NotifyType, task func() error) *sysNotifyTask {
	return &sysNotifyTask{
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

func (sn *SysNotify) RegisterSysNotifyTask(priority int, task *sysNotifyTask) error {
	if task == nil || task.Task == nil {
		return errors.New("task is nil")
	}
	if task.NotifyType == 0 {
		panic("task notify type is 0")
	}
	tasks, _ := sn.taskGroup.LoadOrStore(task.NotifyType, &taskQueue{
		notifyTaskQueue: pqueue.NewMinPriorityQueue[*sysNotifyTask](),
	})
	tasks.notifyTaskLock.Lock()
	defer tasks.notifyTaskLock.Unlock()
	tasks.notifyTaskQueue.Push(priority, task)
	return nil
}

func (sn *SysNotify) waitCbk() {
	log.Info("wait sys notify")
	for s := range sn.c {
		log.Infof("receive sys notify: %v", s)
		switch parseSysNotifyType(s) {
		case NotifyTypeEXIT:
			tq, ok := sn.taskGroup.Load(NotifyTypeEXIT)
			if ok {
				log.Info("task: NotifyTypeEXIT running...")
				runTask(tq)
			}
			return
		case NotifyTypeRELOAD:
			tq, ok := sn.taskGroup.Load(NotifyTypeRELOAD)
			if ok {
				log.Info("task: NotifyTypeRELOAD running...")
				runTask(tq)
			}
		}
	}
	log.Info("task: all done")
}

func (sn *SysNotify) WaitCbk() {
	sn.once.Do(sn.waitCbk)
}
