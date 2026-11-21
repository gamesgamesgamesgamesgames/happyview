-- Which revision of Proposal 0016 a PDS speaks: 'g0' (simplespace),
-- 'g1' (Oct 29) or 'g2' (Nov 12). HappyView calls a host in the newest one it
-- supports. NULL when a probe proved support without naming methods, which
-- says nothing about the revision, and for rows written before this column.
ALTER TABLE happyview_space_pds_support ADD COLUMN generation TEXT;
