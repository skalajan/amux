-- AMUX-2992: board epics. An epic is a type=epic card; every other card can
-- point at one via this column (the semantic id of the epic card, e.g.
-- "AMUX-2992"), so cards roll up under it. NULL = not in any epic. Additive and
-- nullable — existing rows are untouched. The parent link is a plain id, not a
-- foreign key: the board speaks semantic ids and an epic card can be soft-deleted
-- independently (a dangling epic id reads as "no epic", same as NULL).
ALTER TABLE issues ADD COLUMN epic TEXT;
