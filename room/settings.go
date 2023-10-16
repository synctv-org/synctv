package room

import "sync/atomic"

type settings struct {
	hidden       uint32
	needPassword uint32
}

func (s *settings) Hidden() bool {
	return atomic.LoadUint32(&s.hidden) == 1
}

func (s *settings) SetHidden(hidden bool) {
	if hidden {
		atomic.StoreUint32(&s.hidden, 1)
	} else {
		atomic.StoreUint32(&s.hidden, 0)
	}
}

func (s *settings) NeedPassword() bool {
	return atomic.LoadUint32(&s.needPassword) == 1
}

func (s *settings) SetNeedPassword(needPassword bool) {
	if needPassword {
		atomic.StoreUint32(&s.needPassword, 1)
	} else {
		atomic.StoreUint32(&s.needPassword, 0)
	}
}
