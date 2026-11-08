-- Phone link relay (T-010 phase 0/1 review, round 2).
-- Quotas limit *pending* data (design 7.1 / 7.4), so a grant is released when the receiver
-- has consumed the transfer. Called by link-upload with op "release" for the receiving
-- member of the direction only (p2c: the PC, c2p: the phone). Objects still in the bucket
-- after a release are no longer covered by a grant and count at their real size, so an
-- early release or a late upload through an old URL cannot hide data from the quota.

create function public.link_upload_release(p_uid uuid, p_topic text, p_dir text, p_kind text, p_fid text)
returns void
language plpgsql security definer set search_path = '' as $$
begin
  if p_topic is null or p_topic !~ '^nl-[0-9a-f]{32}$' then raise exception 'bad topic'; end if;
  if p_dir is null or p_dir not in ('p2c', 'c2p') then raise exception 'bad dir'; end if;
  if p_kind is null or p_kind not in ('blob', 'mailbox') then raise exception 'bad kind'; end if;
  if p_fid is null or p_fid !~ '^[0-9a-f]{32}$' then raise exception 'bad fid'; end if;
  if not exists (
    select 1 from public.link_members
    where topic = p_topic and member = p_uid and role = case p_dir when 'p2c' then 'pc' else 'phone' end
  ) then
    raise exception 'not member';
  end if;

  perform pg_advisory_xact_lock(hashtext('link-upload-quota'));
  delete from link_private.upload_grants
  where topic = p_topic and dir = p_dir and kind = p_kind and fid = p_fid;
end $$;

revoke execute on function public.link_upload_release(uuid, text, text, text, text)
  from public, anon, authenticated;
grant execute on function public.link_upload_release(uuid, text, text, text, text) to service_role;
