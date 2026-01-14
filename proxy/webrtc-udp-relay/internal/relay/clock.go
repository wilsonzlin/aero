package relay

import "time"

type clock interface {
	Now() time.Time
}
