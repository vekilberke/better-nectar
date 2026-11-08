-- Phone link relay hardening (T-010 phase 0/1 review, round 1).
-- 1. The owner claim is closed until the PC claims: any confirmed email user could
--    otherwise become owner first. Phase 2 opens it, claims, and closes it again.
-- 2. Upload quotas count issued signed upload URLs (grants), not only uploaded objects.
-- 3. The sweep runs as two cron jobs so a failing database part cannot stop the Storage part.

revoke execute on function public.link_owner_claim(bytea) from authenticated;

create table link_private.upload_grants (
  topic text not null references public.link_topics on delete cascade,
  dir text not null check (dir in ('p2c', 'c2p')),
  kind text not null check (kind in ('blob', 'mailbox')),
  fid text not null check (fid ~ '^[0-9a-f]{32}$'),
  chunks integer not null check (chunks between 1 and 64),
  reserved bigint not null check (reserved > 0),
  expires_at timestamptz not null,
  primary key (topic, dir, kind, fid)
);
alter table link_private.upload_grants enable row level security;
revoke all on link_private.upload_grants from public, anon, authenticated;

drop function public.link_upload_authorize(uuid, text, text, text, integer);

-- Reserves room for the signed upload URLs link-upload is about to create. A grant lives as
-- long as those URLs (2 h, plus a margin) and never shrinks while active, so URLs issued
-- earlier for the same fid stay counted. Usage = active grants + uploaded objects that no
-- active grant covers. One advisory lock serializes all calls, so parallel calls cannot
-- pass the same check.
create function public.link_upload_authorize(p_uid uuid, p_topic text, p_dir text, p_kind text, p_fid text, p_count integer)
returns void
language plpgsql security definer set search_path = '' as $$
declare
  chunk_max constant bigint := 4194560;
  v_tag constant text := case p_kind when 'blob' then 'blob' else 'mbox' end;
  prev integer;
  want integer;
  used_all bigint;
  used_dir bigint;
  boxes integer;
begin
  if p_topic is null or p_topic !~ '^nl-[0-9a-f]{32}$' then raise exception 'bad topic'; end if;
  if p_dir is null or p_dir not in ('p2c', 'c2p') then raise exception 'bad dir'; end if;
  if p_kind is null or p_kind not in ('blob', 'mailbox') then raise exception 'bad kind'; end if;
  if p_fid is null or p_fid !~ '^[0-9a-f]{32}$' then raise exception 'bad fid'; end if;
  if p_count is null or p_count < 1 or p_count > 64 or (p_kind = 'mailbox' and p_count <> 1) then
    raise exception 'bad count';
  end if;
  if not exists (
    select 1 from public.link_members
    where topic = p_topic and member = p_uid and role = case p_dir when 'p2c' then 'phone' else 'pc' end
  ) then
    raise exception 'not member';
  end if;

  perform pg_advisory_xact_lock(hashtext('link-upload-quota'));
  delete from link_private.upload_grants where expires_at < now();

  select chunks into prev from link_private.upload_grants
    where topic = p_topic and dir = p_dir and kind = p_kind and fid = p_fid;
  want := greatest(coalesce(prev, 0), p_count);

  with g as (
    select * from link_private.upload_grants
    where not (topic = p_topic and dir = p_dir and kind = p_kind and fid = p_fid)
  ), o as (
    select split_part(name, '/', 1) as topic, split_part(name, '/', 2) as tag,
           split_part(name, '/', 3) as dir, split_part(name, '/', 4) as fid,
           coalesce((metadata->>'size')::bigint, chunk_max) as size
    from storage.objects where bucket_id = 'link-blobs'
  ), free_o as (
    select * from o
    where not (o.topic = p_topic and o.dir = p_dir and o.tag = v_tag and o.fid = p_fid)
      and not exists (
        select 1 from g
        where g.topic = o.topic and g.dir = o.dir and g.fid = o.fid
          and g.kind = case o.tag when 'blob' then 'blob' else 'mailbox' end
      )
  )
  select
    (select coalesce(sum(reserved), 0) from g) + (select coalesce(sum(size), 0) from free_o),
    (select coalesce(sum(reserved), 0) from g where g.topic = p_topic and g.dir = p_dir and g.kind = 'blob')
      + (select coalesce(sum(size), 0) from free_o where free_o.topic = p_topic and free_o.dir = p_dir and free_o.tag = 'blob'),
    (select count(*) from g where g.topic = p_topic and g.dir = p_dir and g.kind = 'mailbox')
      + (select count(*) from free_o where free_o.topic = p_topic and free_o.dir = p_dir and free_o.tag = 'mbox')
  into used_all, used_dir, boxes;

  if used_all + want * chunk_max > 900 * 1024 * 1024 then raise exception 'relay full'; end if;
  if p_kind = 'blob' then
    if used_dir + want * chunk_max > 260 * 1024 * 1024 then raise exception 'quota'; end if;
  elsif boxes >= 5 then
    raise exception 'mailbox full';
  end if;

  insert into link_private.upload_grants (topic, dir, kind, fid, chunks, reserved, expires_at)
  values (p_topic, p_dir, p_kind, p_fid, want, want * chunk_max, now() + interval '2 hours 5 minutes')
  on conflict (topic, dir, kind, fid) do update
    set chunks = excluded.chunks, reserved = excluded.reserved, expires_at = excluded.expires_at;
end $$;

revoke execute on function public.link_upload_authorize(uuid, text, text, text, text, integer)
  from public, anon, authenticated;
grant execute on function public.link_upload_authorize(uuid, text, text, text, text, integer) to service_role;

create or replace function link_private.sweep_db() returns void
language plpgsql security definer set search_path = '' as $$
begin
  delete from public.link_pairings where expires_at < now();
  delete from public.link_queue where created_at < now() - interval '10 minutes';
  delete from link_private.upload_grants where expires_at < now();
  begin
    delete from auth.users u
    where u.is_anonymous
      and u.created_at < now() - interval '24 hours'
      and not exists (select 1 from public.link_owners o where o.uid = u.id)
      and not exists (select 1 from public.link_members m where m.member = u.id);
  exception when others then
    raise warning 'link sweep: anonymous user cleanup failed: %', sqlerrm;
  end;
end $$;
revoke execute on function link_private.sweep_db() from public, anon, authenticated;

select cron.unschedule('link-sweep');
select cron.schedule('link-sweep-db', '*/15 * * * *', 'select link_private.sweep_db()');
select cron.schedule('link-sweep-storage', '*/15 * * * *', $cron$
  select net.http_post(
    url := 'https://qziazuduzrdqnnwdlxqb.supabase.co/functions/v1/link-sweep',
    headers := jsonb_build_object(
      'content-type', 'application/json',
      'x-link-sweep-secret', (select decrypted_secret from vault.decrypted_secrets where name = 'link_sweep_secret')
    ),
    body := '{}'::jsonb,
    timeout_milliseconds := 30000
  );
$cron$);
