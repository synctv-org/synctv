//go:build !windows
// +build !windows

package sysnotify

import (
	"os"
	"os/signal"
	"syscall"
)

func (sn *SysNotify) Init() {
	sn.c = make(chan os.Signal, 1)
	signal.Notify(
		sn.c,
		syscall.SIGHUP,  /*1*/
		syscall.SIGINT,  /*2*/
		syscall.SIGQUIT, /*3*/
		syscall.SIGTERM, /*15*/
		syscall.SIGUSR1, /*10*/
		syscall.SIGUSR2, /*12*/
	)
}

func parseSysNotifyType(s os.Signal) NotifyType {
	switch s {
	case syscall.SIGHUP, syscall.SIGINT, syscall.SIGQUIT, syscall.SIGTERM:
		return NotifyTypeEXIT
	case syscall.SIGUSR1, syscall.SIGUSR2:
		return NotifyTypeRELOAD
	default:
		return 0
	}
}
