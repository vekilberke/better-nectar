-- Phone link relay (T-010 phase 2 review, round 1): a side-effect-free owner check for the
-- PC's connect. The PC used link_cancel_pairing for this, which deleted an open pairing row
-- when it reconnected while a QR was on screen. Raises 'not owner' like the other owner RPCs.

create function link_private.owner_check() returns void
language plpgsql stable security definer set search_path = '' as $$
begin
  if not link_private.is_owner() then raise exception 'not owner'; end if;
end $$;

revoke execute on function link_private.owner_check() from public, anon, authenticated;
grant execute on function link_private.owner_check() to authenticated;

create function public.link_owner_check() returns void
language sql stable security invoker set search_path = '' as $$ select link_private.owner_check() $$;

revoke execute on function public.link_owner_check() from public, anon;
grant execute on function public.link_owner_check() to authenticated;
