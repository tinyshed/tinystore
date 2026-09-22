create table label_values (
    id integer primary key,
    name text not null,
    value text not null,
    unique(name, value)
) strict;

insert into label_values(name, value)
select distinct name, value from postings order by name, value;

create table compact_postings (
    label_id integer not null references label_values(id),
    series_id integer not null references series(id),
    primary key (label_id, series_id)
) strict, without rowid;

insert into compact_postings(label_id, series_id)
select labels.id, postings.series_id
from postings join label_values labels
    on labels.name=postings.name and labels.value=postings.value;

drop table postings;
alter table compact_postings rename to postings;
