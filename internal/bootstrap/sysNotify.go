package bootstrap

import (
	"errors"
	"os"
	"os/signal"
	"sync"
	"syscall"

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

func InitSysNotify() {
	c = make(chan os.Signal, 1)
	signal.Notify(c, syscall.SIGHUP /*1*/, syscall.SIGINT /*2*/, syscall.SIGQUIT /*3*/, syscall.SIGTERM /*15*/, syscall.SIGUSR1 /*10*/, syscall.SIGUSR2 /*12*/)
	WaitCbk = func() {
		once.Do(waitCbk)
	}
}

func waitCbk() {
	log.Info("wait sys notify")
	for s := range c {
		log.Infof("receive sys notify: %v", s)
		switch s {
		case syscall.SIGHUP, syscall.SIGINT, syscall.SIGQUIT, syscall.SIGTERM:
			tq, ok := TaskGroup.Load(NotifyTypeEXIT)
			if ok {
				log.Info("task: NotifyTypeEXIT running...")
				runTask(tq)
			}
			return
		case syscall.SIGUSR1, syscall.SIGUSR2:
			tq, ok := TaskGroup.Load(NotifyTypeRELOAD)
			if ok {
				log.Info("task: NotifyTypeRELOAD running...")
				runTask(tq)
			}
		}
		log.Info("task: all done")
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
