-- NOTE: aesv is self-hosted and sells no plans, so every user holds the paid
-- roles. Many services read "RolesOnUsers" directly, so the grant lives here,
-- where every reader sees it. Billing, organisation and admin roles stay out.
CREATE FUNCTION aesv_grant_every_feature(user_id TEXT) RETURNS void
LANGUAGE sql AS $$
    INSERT INTO "RolesOnUsers" ("userId", "roleId")
    SELECT user_id, role_id
    FROM unnest(ARRAY[
        'professional_subscriber',
        'sub_max',
        'ai_subscriber',
        'editor_user',
        'email_tool'
    ]) AS role_id
    ON CONFLICT DO NOTHING;
$$;

CREATE FUNCTION aesv_grant_every_feature_on_signup() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    PERFORM aesv_grant_every_feature(NEW."id");
    RETURN NEW;
END;
$$;

CREATE TRIGGER aesv_grant_every_feature_on_signup
    AFTER INSERT ON "User"
    FOR EACH ROW EXECUTE FUNCTION aesv_grant_every_feature_on_signup();

SELECT aesv_grant_every_feature("id") FROM "User";
