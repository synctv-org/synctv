package bootstrap

import (
	"errors"
	"os"
	"os/signal"
	"sync"
	"syscall"

	log "github.com/sirupsen/logrus"

	"github.com/zijiren233/gencontainer/pqueue"
)

var (
	c               chan os.Signal
	notifyTaskLock  sync.Mutex
	notifyTaskQueue = pqueue.NewMaxPriorityQueue[*SysNotifyTask]()
	WaitCbk         func()
)

type SysNotifyTask struct {
	Task func() error
	Name string
}

func NewSysNotifyTask(name string, task func() error) *SysNotifyTask {
	return &SysNotifyTask{
		Name: name,
		Task: task,
	}
}

func InitSysNotify() {
	c = make(chan os.Signal, 1)
	signal.Notify(c, syscall.SIGHUP /*1*/, syscall.SIGINT /*2*/, syscall.SIGQUIT /*3*/, syscall.SIGTERM /*15*/)
	WaitCbk = sync.OnceFunc(waitCbk)
}

func waitCbk() {
	log.Info("wait sys notify")
	log.Infof("receive sys notify: %v", <-c)
	notifyTaskLock.Lock()
	defer notifyTaskLock.Unlock()
	log.Infof("task: running...")
	for notifyTaskQueue.Len() > 0 {
		_, task := notifyTaskQueue.Pop()
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
	log.Info("task: all done")
}

func RegisterSysNotifyTask(priority int, task *SysNotifyTask) error {
	if task == nil || task.Task == nil {
		return errors.New("task is nil")
	}
	notifyTaskLock.Lock()
	defer notifyTaskLock.Unlock()
	notifyTaskQueue.Push(priority, task)
	return nil
}
