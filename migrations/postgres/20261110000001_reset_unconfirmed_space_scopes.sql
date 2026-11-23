-- Reset space: scopes on DPoP sessions registered before HappyView began
-- confirming scopes with the authorization server; the next sign-in registers
-- them again.
UPDATE happyview_dpop_sessions SET scopes = 'atproto' WHERE scopes LIKE '%space:%';
