package bootstrap

import (
	"context"
)

type Conf func(*Bootstrap)

func WithTask(f ...Func) Conf {
	return func(b *Bootstrap) {
		b.task = append(b.task, f...)
	}
}

type Bootstrap struct {
	task []Func
}

func New(conf ...Conf) *Bootstrap {
	b := &Bootstrap{}
	for _, c := range conf {
		c(b)
	}
	return b
}

type Func func(context.Context) error

func (b *Bootstrap) Add(f ...Func) *Bootstrap {
	b.task = append(b.task, f...)
	return b
}

func (b *Bootstrap) Run(ctx context.Context) error {
	for _, f := range b.task {
		if err := f(ctx); err != nil {
			return err
		}
	}
	return nil
}
