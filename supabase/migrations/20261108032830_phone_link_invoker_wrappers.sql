-- Phone link relay (T-010 phase 2): advisor lint 0029
-- (authenticated_security_definer_function_executable). The security definer bodies of the
-- client RPCs move into link_private, which the Data API does not expose; public keeps thin
-- security invoker wrappers with the same signatures, so clients call the same endpoints.
-- link_owner_claim follows the same pattern: its wrapper is callable by authenticated, but
-- link_private.owner_claim stays closed, so the claim is still opened only for the PC's
-- one-time claim (grant execute on link_private.owner_claim, claim, revoke).

create function link_private.owner_claim(k bytea) returns void
language plpgsql security definer set search_path = '' as $$
begin
  if auth.uid() is null then raise exception 'no user'; end if;
  if k is null or length(k) <> 32 then raise exception 'bad key'; end if;
  insert into public.link_owners (id, uid, owner_key) values (1, auth.uid(), k);
end $$;

-- mac = HMAC-SHA256(K_owner, utf8('nectar-link v1 owner' || uid || ts))
create function link_private.owner_rebind(ts bigint, mac bytea) returns void
language plpgsql security definer set search_path = '' as $$
declare k bytea;
begin
  if auth.uid() is null then raise exception 'no user'; end if;
  if ts is null or abs(extract(epoch from now()) - ts) > 300 then raise exception 'stale'; end if;
  select owner_key into k from public.link_owners where id = 1;
  if k is null or mac is distinct from
     extensions.hmac(convert_to('nectar-link v1 owner' || auth.uid()::text || ts::text, 'utf8'), k, 'sha256') then
    raise exception 'bad mac';
  end if;
  update public.link_owners set uid = auth.uid() where id = 1;
  update public.link_members set member = auth.uid() where role = 'pc';
end $$;

create function link_private.create_pairing(t text) returns void
language plpgsql security definer set search_path = '' as $$
begin
  if not link_private.is_owner() then raise exception 'not owner'; end if;
  if t is null or t !~ '^nlp-[0-9a-f]{32}$' then raise exception 'bad topic'; end if;
  delete from public.link_pairings where true;
  insert into public.link_pairings (topic, expires_at) values (t, now() + interval '5 minutes');
end $$;

create function link_private.cancel_pairing() returns void
language plpgsql security definer set search_path = '' as $$
begin
  if not link_private.is_owner() then raise exception 'not owner'; end if;
  delete from public.link_pairings where true;
end $$;

create function link_private.create_device(pair_topic text, t text, phone uuid, rk bytea) returns void
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

create function link_private.remove_device(t text) returns void
language plpgsql security definer set search_path = '' as $$
begin
  if not link_private.is_owner() then raise exception 'not owner'; end if;
  delete from public.link_topics where topic = t;
end $$;

-- mac = HMAC-SHA256(K_rejoin, utf8('nectar-link v1 rejoin' || topic || uid || ts))
create function link_private.rejoin(t text, ts bigint, mac bytea) returns void
language plpgsql security definer set search_path = '' as $$
declare k bytea;
begin
  if auth.uid() is null then raise exception 'no user'; end if;
  if ts is null or abs(extract(epoch from now()) - ts) > 300 then raise exception 'stale'; end if;
  select rejoin_key into k from public.link_topics where topic = t;
  if k is null or mac is distinct from
     extensions.hmac(convert_to('nectar-link v1 rejoin' || t || auth.uid()::text || ts::text, 'utf8'), k, 'sha256') then
    raise exception 'bad mac';
  end if;
  update public.link_members set member = auth.uid() where topic = t and role = 'phone';
end $$;

revoke execute on function link_private.owner_claim(bytea), link_private.owner_rebind(bigint, bytea),
  link_private.create_pairing(text), link_private.cancel_pairing(),
  link_private.create_device(text, text, uuid, bytea), link_private.remove_device(text),
  link_private.rejoin(text, bigint, bytea)
  from public, anon, authenticated;
grant execute on function link_private.owner_rebind(bigint, bytea),
  link_private.create_pairing(text), link_private.cancel_pairing(),
  link_private.create_device(text, text, uuid, bytea), link_private.remove_device(text),
  link_private.rejoin(text, bigint, bytea)
  to authenticated;

create or replace function public.link_owner_claim(k bytea) returns void
language sql security invoker set search_path = '' as $$ select link_private.owner_claim(k) $$;
create or replace function public.link_owner_rebind(ts bigint, mac bytea) returns void
language sql security invoker set search_path = '' as $$ select link_private.owner_rebind(ts, mac) $$;
create or replace function public.link_create_pairing(t text) returns void
language sql security invoker set search_path = '' as $$ select link_private.create_pairing(t) $$;
create or replace function public.link_cancel_pairing() returns void
language sql security invoker set search_path = '' as $$ select link_private.cancel_pairing() $$;
create or replace function public.link_create_device(pair_topic text, t text, phone uuid, rk bytea) returns void
language sql security invoker set search_path = '' as $$ select link_private.create_device(pair_topic, t, phone, rk) $$;
create or replace function public.link_remove_device(t text) returns void
language sql security invoker set search_path = '' as $$ select link_private.remove_device(t) $$;
create or replace function public.link_rejoin(t text, ts bigint, mac bytea) returns void
language sql security invoker set search_path = '' as $$ select link_private.rejoin(t, ts, mac) $$;

revoke execute on function public.link_owner_claim(bytea), public.link_owner_rebind(bigint, bytea),
  public.link_create_pairing(text), public.link_cancel_pairing(),
  public.link_create_device(text, text, uuid, bytea), public.link_remove_device(text),
  public.link_rejoin(text, bigint, bytea)
  from public, anon;
grant execute on function public.link_owner_claim(bytea), public.link_owner_rebind(bigint, bytea),
  public.link_create_pairing(text), public.link_cancel_pairing(),
  public.link_create_device(text, text, uuid, bytea), public.link_remove_device(text),
  public.link_rejoin(text, bigint, bytea)
  to authenticated;
