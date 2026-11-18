-- +goose Up
ALTER TABLE invoices DROP CONSTRAINT invoices_pkey;
ALTER TABLE invoices ADD PRIMARY KEY (course_enrollment_id, id);

-- +goose Down
DELETE FROM invoices AS duplicate
USING invoices AS kept
WHERE duplicate.id = kept.id
    AND duplicate.course_enrollment_id > kept.course_enrollment_id;
ALTER TABLE invoices DROP CONSTRAINT invoices_pkey;
ALTER TABLE invoices ADD PRIMARY KEY (id);
