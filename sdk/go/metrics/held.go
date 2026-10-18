package metrics

import (
	"context"
	"time"

	"tinyshed.org/tinystore"
	"tinyshed.org/tinystore/internal/core"
	"tinyshed.org/tinystore/internal/wire"
)

// PageOption is how a page of names, labels or values reads: Prefix, After, PageLimit.
type PageOption interface{ page(*wire.MetricsPageAsked) }

func (c Condition) page(p *wire.MetricsPageAsked) { p.Prefix = c.wire.Prefix }

type after string

// After starts a page after the page before's Next.
func After(next string) PageOption { return after(next) }

func (a after) page(p *wire.MetricsPageAsked) {
	if a != "" {
		next := string(a)
		p.After = &next
	}
}

type pageLimit int

// PageLimit is how many a page holds: 1000 unless given, 10,000 at most.
func PageLimit(n int) PageOption { return pageLimit(n) }

func (n pageLimit) page(p *wire.MetricsPageAsked) { limit := uint64(max(int(n), 0)); p.Limit = &limit }

func pageAsked(opts []PageOption) wire.MetricsPageAsked {
	var asked wire.MetricsPageAsked
	for _, opt := range opts {
		opt.page(&asked)
	}
	return asked
}

// Name is a name the store holds samples of.
type Name struct {
	Name        string
	Kind        Kind
	SeriesCount int
	Unit, Help  string
	Keep        time.Duration
	// RetainedFrom is its oldest sample, or where its keep begins while it
	// holds samples from before, which no read finds.
	RetainedFrom time.Time
	Newest       time.Time
}

// NamePage is a page of names and where the next starts: "" on the last.
type NamePage struct {
	Names []Name
	Next  string
}

// TextPage is a page of a name's labels or a label's values, and where the next starts.
type TextPage struct {
	Texts []string
	Next  string
}

// Names is a page of the names the store holds samples of, in the byte
// order of their text; none of it reads a sample.
func Names(ctx context.Context, store *tinystore.Store, opts ...PageOption) (NamePage, error) {
	asked := pageAsked(opts)
	answer, err := linkOf(store).Call(ctx, core.Reading, wire.MethodMetricsNames, wire.Encode(&asked))
	if err != nil {
		return NamePage{}, err
	}
	var page wire.MetricsNamePage
	if err := decode(answer, &page); err != nil {
		return NamePage{}, err
	}
	out := NamePage{Names: make([]Name, len(page.Names))}
	for i, each := range page.Names {
		out.Names[i] = Name{
			Name: each.Name, Kind: Kind(each.Kind), SeriesCount: int(each.SeriesCount), Unit: each.Unit, Help: each.Help,
			Keep:         time.Duration(each.Keep) * time.Millisecond,
			RetainedFrom: time.UnixMilli(each.RetainedFrom).UTC(), Newest: time.UnixMilli(each.Newest).UTC(),
		}
	}
	if page.Next != nil {
		out.Next = *page.Next
	}
	return out, nil
}

// LabelNames is a page of a name's label names, but the store's own.
func LabelNames(ctx context.Context, store *tinystore.Store, name string, opts ...PageOption) (TextPage, error) {
	asked := pageAsked(opts)
	asked.Name = &name
	return textPage(ctx, store, wire.MethodMetricsLabelNames, asked)
}

// LabelValues is a page of the values a label of a name's series holds.
func LabelValues(ctx context.Context, store *tinystore.Store, name, label string, opts ...PageOption) (TextPage, error) {
	asked := pageAsked(opts)
	asked.Name, asked.Label = &name, &label
	return textPage(ctx, store, wire.MethodMetricsLabelValues, asked)
}

func textPage(ctx context.Context, store *tinystore.Store, method uint16, asked wire.MetricsPageAsked) (TextPage, error) {
	answer, err := linkOf(store).Call(ctx, core.Reading, method, wire.Encode(&asked))
	if err != nil {
		return TextPage{}, err
	}
	var page wire.MetricsTextPage
	if err := decode(answer, &page); err != nil {
		return TextPage{}, err
	}
	out := TextPage{Texts: page.Texts}
	if page.Next != nil {
		out.Next = *page.Next
	}
	return out, nil
}

// Description is a name's unit and help; both empty when nothing described it.
type Description struct {
	Unit, Help string
}

// Describe sets a name's unit and help in place of what it had: what is not
// given is removed. They belong to the name, so a Delete of its series leaves them.
func Describe(ctx context.Context, store *tinystore.Store, name string, description Description) error {
	body := wire.MetricsDescription{Name: name, Unit: description.Unit, Help: description.Help}
	_, err := linkOf(store).Call(ctx, core.Writing, wire.MethodMetricsDescribe, wire.Encode(&body))
	return err
}

// DescriptionOf is a name's unit and help.
func DescriptionOf(ctx context.Context, store *tinystore.Store, name string) (Description, error) {
	answer, err := linkOf(store).Call(ctx, core.Reading, wire.MethodMetricsDescription, wire.Encode(&wire.MetricsNameAsked{Name: name}))
	if err != nil {
		return Description{}, err
	}
	var described wire.MetricsDescription
	err = decode(answer, &described)
	return Description{Unit: described.Unit, Help: described.Help}, err
}

// Delete removes the one series of exactly this name and these labels,
// whether its samples still read or not, and says whether the store held
// it; nil labels are the series without any.
func Delete(ctx context.Context, store *tinystore.Store, name string, labels Labels) (bool, error) {
	body := wire.MetricsSeriesNamed{Name: name, Labels: labels}
	answer, err := linkOf(store).Call(ctx, core.Writing, wire.MethodMetricsDelete, wire.Encode(&body))
	if err != nil {
		return false, err
	}
	var deleted wire.MetricsDeleted
	err = decode(answer, &deleted)
	return deleted.Found, err
}

// Damage is a series whose bytes no longer read, which Delete removes.
type Damage struct {
	Name   string
	Labels Labels
	Error  string
}

// Damaged is the series whose bytes no longer read, 1000 at most, each with
// what broke; deleting them shows the next.
func Damaged(ctx context.Context, store *tinystore.Store) ([]Damage, error) {
	answer, err := linkOf(store).Call(ctx, core.Reading, wire.MethodMetricsDamaged, wire.Encode(&wire.Empty{}))
	if err != nil {
		return nil, err
	}
	var damages wire.MetricsDamages
	if err := decode(answer, &damages); err != nil {
		return nil, err
	}
	out := make([]Damage, len(damages.Damages))
	for i, each := range damages.Damages {
		out[i] = Damage{Name: each.Name, Labels: each.Labels, Error: each.Error}
	}
	return out, nil
}

// Flush sends what every instrument changed and has the store write its
// totals now, as it does every 15 seconds. One the store cannot be reached
// for fails as a call does, and what it carried goes once it can be; what
// the store refused since the flush before, and what a store that started
// again may have counted, fail it, Invalid and OutcomeUnknown, each once.
func Flush(ctx context.Context, store *tinystore.Store) error {
	if sent, err := instrumentsOf(store).flush(ctx); sent {
		return err
	}
	_, err := linkOf(store).Call(ctx, core.Writing, wire.MethodMetricsFlush, wire.Encode(&wire.Empty{}))
	return err
}
