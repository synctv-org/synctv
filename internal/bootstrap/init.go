package bootstrap

import (
	"context"
)

type BootstrapConf func(*Bootstrap)

func WithContext(ctx context.Context) BootstrapConf {
	return func(b *Bootstrap) {
		b.ctx = ctx
	}
}

func WithTask(f ...BootstrapFunc) BootstrapConf {
	return func(b *Bootstrap) {
		b.task = append(b.task, f...)
	}
}

type Bootstrap struct {
	ctx  context.Context
	task []BootstrapFunc
}

func New(conf ...BootstrapConf) *Bootstrap {
	b := &Bootstrap{}
	for _, c := range conf {
		c(b)
	}
	return b
}

type BootstrapFunc func(context.Context) error

func (b *Bootstrap) Add(f ...BootstrapFunc) *Bootstrap {
	b.task = append(b.task, f...)
	return b
}

func (b *Bootstrap) Run() error {
	for _, f := range b.task {
		if err := f(b.ctx); err != nil {
			return err
		}
	}
	return nil
}
