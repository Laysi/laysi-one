-- The working seconds a leave request covers on each of its dates.
--
-- A request's hours and days are settled when it is sent, from the working
-- window and the work calendar as they stood then. How those days fall on the
-- request's dates decides which payroll period and which annual leave year
-- each one belongs to, and that was being recomputed from the current window
-- on every read: clearing the start or end of the working day moved a request
-- that crosses a work anniversary wholly onto its first date, and with it a
-- day from one annual leave year into the other. The split is now written with
-- the request and read back as it was.
--
-- A request whose dates had no working time when it was recorded has no rows,
-- and is read as sitting wholly on its first date.

CREATE TABLE leave_request_day (
    leave_request_id integer NOT NULL,
    on_date date NOT NULL,
    seconds integer NOT NULL,
    CONSTRAINT leave_request_day_pkey PRIMARY KEY (leave_request_id, on_date),
    CONSTRAINT leave_request_day_request_fkey FOREIGN KEY (leave_request_id)
        REFERENCES leave_request(id) ON DELETE CASCADE,
    CONSTRAINT leave_request_day_seconds CHECK (((seconds > 0) AND (seconds <= 86400)))
);

-- Requests sent before this table existed are split by today's working window
-- and calendar, which is the split every read was computing until now. The
-- arithmetic is `WorkSchedule::leave_seconds_by_date`: on each working date,
-- the overlap of the request with the working window less the part of the
-- break inside that overlap, each in whole seconds with any fraction dropped as
-- `num_seconds()` drops it; a date the calendar does not list is a working day
-- from Monday to Friday. A setting that is blank or not a valid HH:MM is no
-- setting, as it is when the server reads it, and without both ends of the
-- working day nothing is written: there is nothing to split by, and
-- greatest() and least() would skip the missing end rather than give NULL.
WITH clock AS (
    SELECT max(t) FILTER (WHERE code = 'work_start') AS work_start,
           max(t) FILTER (WHERE code = 'work_end') AS work_end,
           max(t) FILTER (WHERE code = 'break_start') AS break_start,
           max(t) FILTER (WHERE code = 'break_end') AS break_end
      FROM (
        SELECT code,
               CASE WHEN btrim(value) ~ '^([01][0-9]|2[0-3]):[0-5][0-9]$'
                    THEN btrim(value)::time
               END AS t
          FROM company_setting
         WHERE code = ANY (ARRAY['work_start'::text, 'work_end'::text,
                                 'break_start'::text, 'break_end'::text])
      ) s
)
INSERT INTO leave_request_day (leave_request_id, on_date, seconds)
SELECT l.id, d.on_date, w.seconds
  FROM leave_request l
 CROSS JOIN clock c
 CROSS JOIN LATERAL generate_series(l.from_on, l.to_on, interval '1 day') AS g(day)
 CROSS JOIN LATERAL (SELECT g.day::date AS on_date) d
  LEFT JOIN work_calendar k ON k.on_date = d.on_date
 CROSS JOIN LATERAL (
    SELECT greatest(l.from_at, d.on_date + c.work_start) AS open,
           least(l.to_at, d.on_date + c.work_end) AS close
 ) o
 CROSS JOIN LATERAL (
    SELECT (greatest(trunc(extract(epoch FROM o.close - o.open)), 0)
            - CASE WHEN c.break_end > c.break_start
                   THEN greatest(trunc(extract(epoch FROM least(o.close, d.on_date + c.break_end)
                                                    - greatest(o.open, d.on_date + c.break_start))), 0)
                   ELSE 0
              END)::integer AS seconds
 ) w
 WHERE c.work_start IS NOT NULL
   AND c.work_end IS NOT NULL
   AND COALESCE(k.kind,
                CASE extract(isodow FROM d.on_date)
                     WHEN 6 THEN 'rest_day'
                     WHEN 7 THEN 'regular_leave'
                     ELSE 'workday'
                END) = 'workday'
   AND w.seconds > 0;
