alter table label_values add column posting_count integer not null default 0 check (posting_count >= 0);
update label_values set posting_count = (select count(*) from postings where postings.label_id = label_values.id);
