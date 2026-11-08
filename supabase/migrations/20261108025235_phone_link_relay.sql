-- Phone link relay (docs/phone-link-design.md section 8).
-- Clients never touch the link_* tables: RLS is on with no table policies, and every
-- client action goes through a security definer RPC. Policy helpers live in the
-- unexposed schema link_private; service-role-only RPCs back the Edge Functions.

create extension if not exists pgcrypto with schema extensions;
create extension if not exists pg_cron;
create extension if not exists pg_net with schema extensions;

create schema if not exists link_private;
revoke all on schema link_private from public, anon;
grant usage on schema link_private to authenticated;

create table public.link_owners (
  id smallint primary key default 1 check (id = 1),
  uid uuid unique,
  owner_key bytea not null check (length(owner_key) = 32)
);
create table public.link_topics (
  topic text primary key check (topic ~ '^nl-[0-9a-f]{32}$'),
  rejoin_key bytea not null check (length(rejoin_key) = 32),
  created_at timestamptz not null default now()
);
create table public.link_members (
  topic text not null references public.link_topics on delete cascade,
  role text not null check (role in ('pc', 'phone')),
  member uuid not null,
  primary key (topic, role)
);
create index link_members_member_idx on public.link_members (member);
create table public.link_pairings (
  topic text primary key check (topic ~ '^nlp-[0-9a-f]{32}$'),
  expires_at timestamptz not null
);
create table public.link_queue (
  id bigint generated always as identity primary key,
  topic text not null references public.link_topics on delete cascade,
  sender text not null check (sender in ('pc', 'phone')),
  body text not null check (length(body) <= 90000),
  created_at timestamptz not null default now()
);
create index link_queue_topic_idx on public.link_queue (topic, id);

alter table public.link_owners   enable row level security;
alter table public.link_topics   enable row level security;
alter table public.link_members  enable row level security;
alter table public.link_pairings enable row level security;
alter table public.link_queue    enable row level security;
revoke all on public.link_owners, public.link_topics, public.link_members,
  public.link_pairings, public.link_queue from anon, authenticated;

-- Policy helpers. They run with the caller's role, so authenticated needs EXECUTE,
-- but link_private is not exposed through the Data API.

create function link_private.is_owner() returns boolean
language sql stable security definer set search_path = '' as $$
  select exists (select 1 from public.link_owners where uid = auth.uid());
$$;

create function link_private.is_member(t text) returns boolean
language sql stable security definer set search_path = '' as $$
  select exists (select 1 from public.link_members where topic = t and member = auth.uid());
$$;

create function link_private.can_use(t text) returns boolean
language sql stable security definer set search_path = '' as $$
  select exists (select 1 from public.link_members where topic = t and member = auth.uid())
      or exists (select 1 from public.link_pairings where topic = t and expires_at > now());
$$;

-- Client RPCs.

create function public.link_owner_claim(k bytea) returns void
language plpgsql security definer set search_path = '' as $$
begin
  if auth.uid() is null then raise exception 'no user'; end if;
  if k is null or length(k) <> 32 then raise exception 'bad key'; end if;
  insert into public.link_owners (id, uid, owner_key) values (1, auth.uid(), k);
end $$;

-- mac = HMAC-SHA256(K_owner, utf8('nectar-link v1 owner' || uid || ts))
create function public.link_owner_rebind(ts bigint, mac bytea) returns void
language plpgsql security definer set search_path = '' as $$
declare k bytea;
begin
  if auth.uid() is null then raise exception 'no user'; end if;
  if abs(extract(epoch from now()) - ts) > 300 then raise exception 'stale'; end if;
  select owner_key into k from public.link_owners where id = 1;
  if k is null or mac is distinct from
     extensions.hmac(convert_to('nectar-link v1 owner' || auth.uid()::text || ts::text, 'utf8'), k, 'sha256') then
    raise exception 'bad mac';
  end if;
  update public.link_owners set uid = auth.uid() where id = 1;
  update public.link_members set member = auth.uid() where role = 'pc';
end $$;

create function public.link_create_pairing(t text) returns void
language plpgsql security definer set search_path = '' as $$
begin
  if not link_private.is_owner() then raise exception 'not owner'; end if;
  if t is null or t !~ '^nlp-[0-9a-f]{32}$' then raise exception 'bad topic'; end if;
  delete from public.link_pairings where true;
  insert into public.link_pairings (topic, expires_at) values (t, now() + interval '5 minutes');
end $$;

create function public.link_cancel_pairing() returns void
language plpgsql security definer set search_path = '' as $$
begin
  if not link_private.is_owner() then raise exception 'not owner'; end if;
  delete from public.link_pairings where true;
end $$;

create function public.link_create_device(pair_topic text, t text, phone uuid, rk bytea) returns void
language plpgsql security definer set search_path = '' as $$
begin
  if not link_private.is_owner() then raise exception 'not owner'; end if;
  if t is null or t !~ '^nl-[0-9a-f]{32}$' then raise exception 'bad topic'; end if;
  if phone is null or phone = auth.uid() then raise exception 'bad phone'; end if;
  if rk is null or length(rk) <> 32 then raise exception 'bad key'; end if;
  delete from public.link_pairings where topic = pair_topic and expires_at > now();
  if not found then raise exception 'no offer'; end if;
  if (select count(*) from public.link_topics) >= 10 then raise exception 'too many devices'; end if;
  insert into public.link_topics (topic, rejoin_key) values (t, rk);
  insert into public.link_members (topic, role, member) values (t, 'pc', auth.uid()), (t, 'phone', phone);
end $$;

create function public.link_remove_device(t text) returns void
language plpgsql security definer set search_path = '' as $$
begin
  if not link_private.is_owner() then raise exception 'not owner'; end if;
  delete from public.link_topics where topic = t;
end $$;

-- mac = HMAC-SHA256(K_rejoin, utf8('nectar-link v1 rejoin' || topic || uid || ts))
create function public.link_rejoin(t text, ts bigint, mac bytea) returns void
language plpgsql security definer set search_path = '' as $$
declare k bytea;
begin
  if auth.uid() is null then raise exception 'no user'; end if;
  if abs(extract(epoch from now()) - ts) > 300 then raise exception 'stale'; end if;
  select rejoin_key into k from public.link_topics where topic = t;
  if k is null or mac is distinct from
     extensions.hmac(convert_to('nectar-link v1 rejoin' || t || auth.uid()::text || ts::text, 'utf8'), k, 'sha256') then
    raise exception 'bad mac';
  end if;
  update public.link_members set member = auth.uid() where topic = t and role = 'phone';
end $$;

-- Service-role RPCs used by the Edge Functions.

create function public.link_upload_authorize(p_uid uuid, p_topic text, p_dir text, p_kind text, p_count integer)
returns void
language plpgsql security definer set search_path = '' as $$
declare
  chunk_max constant bigint := 4194560;
  pending bigint;
  total bigint;
  boxes integer;
begin
  if p_topic is null or p_topic !~ '^nl-[0-9a-f]{32}$' then raise exception 'bad topic'; end if;
  if p_dir not in ('p2c', 'c2p') then raise exception 'bad dir'; end if;
  if p_kind not in ('blob', 'mailbox') then raise exception 'bad kind'; end if;
  if p_count is null or p_count < 1 or p_count > 64 or (p_kind = 'mailbox' and p_count <> 1) then
    raise exception 'bad count';
  end if;
  if not exists (
    select 1 from public.link_members
    where topic = p_topic and member = p_uid and role = case p_dir when 'p2c' then 'phone' else 'pc' end
  ) then
    raise exception 'not member';
  end if;

  select coalesce(sum((metadata->>'size')::bigint), 0) into total
    from storage.objects where bucket_id = 'link-blobs';
  if total + p_count * chunk_max > 900 * 1024 * 1024 then raise exception 'relay full'; end if;

  if p_kind = 'blob' then
    select coalesce(sum((metadata->>'size')::bigint), 0) into pending
      from storage.objects
      where bucket_id = 'link-blobs' and name like p_topic || '/blob/' || p_dir || '/%';
    if pending + p_count * chunk_max > 260 * 1024 * 1024 then raise exception 'quota'; end if;
  else
    select count(*) into boxes
      from storage.objects
      where bucket_id = 'link-blobs' and name like p_topic || '/mbox/' || p_dir || '/%';
    if boxes >= 5 then raise exception 'mailbox full'; end if;
  end if;
end $$;

create function public.link_sweep_check(s text) returns boolean
language sql stable security definer set search_path = '' as $$
  select exists (
    select 1 from vault.decrypted_secrets
    where name = 'link_sweep_secret' and decrypted_secret = s
  );
$$;

create function public.link_sweep_expired(lim integer) returns setof text
language sql stable security definer set search_path = '' as $$
  select name from storage.objects
  where bucket_id = 'link-blobs' and created_at < now() - interval '24 hours'
  order by created_at
  limit least(greatest(lim, 1), 1000);
$$;

-- Database part of the sweep; runs from pg_cron directly.
create function link_private.sweep_db() returns void
language plpgsql security definer set search_path = '' as $$
begin
  delete from public.link_pairings where expires_at < now();
  delete from public.link_queue where created_at < now() - interval '10 minutes';
  delete from auth.users u
  where u.is_anonymous
    and u.created_at < now() - interval '24 hours'
    and not exists (select 1 from public.link_owners o where o.uid = u.id)
    and not exists (select 1 from public.link_members m where m.member = u.id);
end $$;

revoke execute on all functions in schema link_private from public, anon, authenticated;
grant execute on function link_private.is_owner(), link_private.is_member(text),
  link_private.can_use(text) to authenticated;

revoke execute on function public.link_owner_claim(bytea), public.link_owner_rebind(bigint, bytea),
  public.link_create_pairing(text), public.link_cancel_pairing(),
  public.link_create_device(text, text, uuid, bytea), public.link_remove_device(text),
  public.link_rejoin(text, bigint, bytea),
  public.link_upload_authorize(uuid, text, text, text, integer),
  public.link_sweep_check(text), public.link_sweep_expired(integer)
  from public, anon, authenticated;
grant execute on function public.link_owner_claim(bytea), public.link_owner_rebind(bigint, bytea),
  public.link_create_pairing(text), public.link_cancel_pairing(),
  public.link_create_device(text, text, uuid, bytea), public.link_remove_device(text),
  public.link_rejoin(text, bigint, bytea) to authenticated;
grant execute on function public.link_upload_authorize(uuid, text, text, text, integer),
  public.link_sweep_check(text), public.link_sweep_expired(integer) to service_role;

-- Realtime: private broadcast/presence only on topics the caller may use.
create policy link_read on realtime.messages for select to authenticated
  using (extension in ('broadcast', 'presence') and (select link_private.can_use(realtime.topic())));
create policy link_write on realtime.messages for insert to authenticated
  with check (extension in ('broadcast', 'presence') and (select link_private.can_use(realtime.topic())));

-- Storage: private bucket, one object per encrypted 4 MiB chunk. Clients read and delete
-- inside their own topic; there is no insert policy, uploads use signed URLs from link-upload.
insert into storage.buckets (id, name, public, file_size_limit)
values ('link-blobs', 'link-blobs', false, 4194560)
on conflict (id) do nothing;

create policy link_blobs_read on storage.objects for select to authenticated
  using (bucket_id = 'link-blobs' and (select link_private.is_member((storage.foldername(name))[1])));
create policy link_blobs_delete on storage.objects for delete to authenticated
  using (bucket_id = 'link-blobs' and (select link_private.is_member((storage.foldername(name))[1])));

-- Sweep every 15 minutes: database rows here, Storage objects through link-sweep
-- (objects must be removed through the Storage API). The shared secret stays in Vault.
select vault.create_secret(encode(extensions.gen_random_bytes(32), 'hex'), 'link_sweep_secret',
  'Shared secret between pg_cron and the link-sweep Edge Function');

select cron.schedule('link-sweep', '*/15 * * * *', $cron$
  select link_private.sweep_db();
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
