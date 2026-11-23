# The AVA's GraphQL API, as observed on 2026-11-18

Everything here was observed in a real, logged-in session of the student portal on 2026-11-18,
by reading the site's own JavaScript and the requests the pages send. No personal data is kept:
identifiers, names and values are replaced by types, masks (`9` for every digit) or fictitious
examples. Each item says how it was established:

- **verified**: seen working (a request that returned data, or a value seen in a real response).
- **from the site's code**: read in the site's JavaScript, not exercised.
- **unverified**: an assumption the implementation must treat defensively.

## Endpoint and headers

- One endpoint for everything below: `POST https://graphql.ampli.com.br/` with a JSON body
  `{"operationName", "query", "variables"}` and `content-type: application/json`. **verified**
- Before login the site sends `authorization: Basic cG9ydGFsOnNlY3JldA==` (base64 of
  `portal:secret`). It is a public credential of the web client, embedded in the site's
  JavaScript and identical for every visitor. Without it `LoginStudent` answers HTTP 500 with
  `Cannot read properties of undefined (reading 'grantedAuthorities')`. **verified**
- After login every request sends `authorization: Bearer <accessToken>`. **verified**
- The spike also sent `origin: https://www.anhanguera.com` and `referer:
  https://www.anhanguera.com/`. Keep them; it is not known whether they are required.
  **unverified**
- The site's own static files (`/ava-monolith/static/js/...`) answer 403 to a non-browser HTTP
  client. The GraphQL endpoint does not (the spike used Node's `fetch`). **verified**

## Login

`graphql/LoginStudent.graphql`. Variables: `username` = the CPF, digits only; `password`;
`deviceId` = any string, the site uses a UUID v4 stored in `localStorage.deviceId`.

- Answer: `accessToken` (string), `tokenType` (`"Bearer"`), `expiresIn` (`25200`, seconds, so 7
  hours), `scope` (`"read,write"`), `deviceWhiteListed` (boolean). **verified**
- No captcha is needed. The site runs reCAPTCHA v3 only on the CPF step
  (`GetStudentUserLoginInfo(username, captcha: CaptchaInput!)`), which the monitor does not call.
  `LoginStudent` has no captcha variable and worked from a script. **verified**
- A device that was never confirmed gets `deviceWhiteListed: false` **and still a working
  token**: `sofiaGetMe { id }` and `me { id }` returned data with it. The e-mail/SMS code is only
  asked by the site's login screen (`SendCode`, `ValidateCode`, `WhitelistDevice`). The monitor
  never calls those. **verified**
- There is no refresh token. Log in again when the token expires or a call answers with an
  authentication error. **verified** (no such field exists in the answer)
- The error for a wrong password is not known. **unverified**. Treat any GraphQL error or non-200
  answer from `LoginStudent` as a rejected login (see the design for the backoff).
- The site's front end treats GraphQL error codes `UNAUTHORIZED` and `UNAUTHENTICATED`, and HTTP
  401/403, as "session over, log out". **from the site's code**

## Student id

`graphql/StudentId.graphql`: `sofiaGetMe { id }`. The site passes exactly this `id` as
`studentId` to the course list. **verified**

## Course list

`graphql/CourseEnrollments.graphql`, variable `studentId`.

```
data: [ { id: string, status: string, course: { name: string } } ]
```

- `id` is the course enrollment id (shape `KRT-<uuid>`), used by every query below. **verified**
- `status` seen: `ATTENDING`. **verified**
- `course.name` example (the owner's course, not secret): `Análise e desenvolvimento de sistemas`.
  **verified**

## Subjects and their deadlines

`graphql/SubjectDeadlines.graphql`, variable `courseEnrollmentId`. A trimmed copy of the site's
`GetCourseEnrollmentForPages`, which the home page sends.

```
data: {
  id: string,
  subjectEnrollments: [ {
    id: string,
    status: string,                 seen: PASSED, FAILED, IN_PROGRESS
    subject: { name: string },      upper case, e.g. "SISTEMAS OPERACIONAIS"
    assignments: [ {
      id: string,
      status: string,               seen: AVAILABLE, DONE, NOT_AVAILABLE
      detail: {
        id: string,
        startDate: string,          format 9999-99-99 (a date, no time, no zone)
        dueDate: string,            format 9999-99-99
        display: {
          name: string,             seen: "Trabalho", "Prova", "Prova online",
                                    "Prova de Segunda chamada", "Prova de Recuperação"
          periodTitle: string,      seen: "Período de trabalho", "Período de prova",
                                    "Período de segunda chamada", "Período de recuperação"
          periodDates: string       format "99/99/9999 - 99/99/9999" (start - end, dd/mm/yyyy)
        },
        config: { name: string, type: string },   type seen: ASSIGNMENT, EXAM
        attempts: [ { id: string } ]
      }
    } ]
  } ]
}
```

All of the above **verified** on 2026-11-18, except that the trimmed query itself was not sent:
the site sends the full `GetCourseEnrollmentForPages`, of which this is a subset of fields.
Asking for fewer fields is standard GraphQL, but the first live run must confirm it (the `probe`
command in the design exists for that).

The query also asks for `progress { details { type completed quantity } }`. Seen per in-progress
subject: `type` in `ASSIGNMENT`, `EXAM`, `ATTENDANCE`, `QUIZ`; `quantity` a number; `completed` a
number **or `null`** (treat `null` as 0). `ATTENDANCE` is the lessons watched ("Progresso em
Aula"). **verified**

Rules carried over from the owner's study-plan tool, which reads the same data:

- Only subjects with `status: IN_PROGRESS` matter.
- An assessment is pending when its `attempts` list is empty and its `status` is not `DONE`. A
  zero grade does not mean pending.
- The effective deadline is the earlier of `detail.dueDate` and the end date of
  `display.periodDates`. The period usually closes first.

## Unit quizzes

`graphql/UnitAssignments.graphql`, variable `subjectEnrollmentId` (the `id` of a subject
enrollment above). A trimmed copy of the site's `FindLearningUnitEnrollments`, which a subject's
page sends. One call per subject.

```
data: [ {                           one per learning unit
  id, title, order: number,
  status: string,                   seen: PUBLISHED
  startDate, endDate: string,       format 9999-99-99
  assignments: [ {
    grade: number, maxGrade: number,
    detail: {
      assignmentId: string,
      dueDate, startDate: string,   format 9999-99-99
      config: {
        name: string,
        type: string,               seen: QUIZ, ATTENDANCE
        numberOfAttempts: number or null
      },
      attempts: [ { assignmentAttempId: string, attemptDate: string } ]   format 9999-99-99T99:99:99Z
    }
  } ]
} ]
```

**verified** on four in-progress subjects on 2026-11-18: two had 4 units with 8 assignments each
(`QUIZ` and `ATTENDANCE`), two had units with no assignments. An empty `assignments` list is
normal. The trimmed query itself was not sent live.

- A `QUIZ` is the unit exercise. Pending when `attempts` is empty.
- An `ATTENDANCE` entry is lesson progress, not something to submit.

## Calendar

`graphql/Calendar.graphql`, variables `courseEnrollmentId`, `date` and `eventCategories`. A
trimmed copy of the site's `GetEnrollmentCalendarV2`, which the home page sends.

- The site sends `date` = today (`2026-11-18`) and `eventCategories` =
  `["ACADEMIC_DATES", "UNIQUE_EVENTS", "SECRETARIAT", "FINANCIAL", "HOLIDAYS"]`. **verified**
- The answer covers **the whole term**, not just the month of `date`: 26 events from `2026-08-03`
  to `2027-01-31`. **verified**
- Shape: `data: [ { eventId, category, subCategory, title, description, eventDate, startDate,
  endDate (all three 9999-99-99), isCanceled: boolean } ]`. **verified**
- Seen on 2026-11-18, all with `category: ACADEMIC_DATES`: **verified**
  - `subCategory: ASSIGNMENT` (15), `title` = the subject name in upper case, `description`
    `"Trabalho"`.
  - `subCategory: EXAM_PERIOD` (4), `title` = subject name, `description` `"Primeira chamada"`
    or `"Segunda chamada"`.
  - `subCategory: STUDY_PERIOD` (6), `title` = subject name. The deadline to watch the lessons.
  - `subCategory: ACADEMIC_HOLIDAYS` (1), `title` `"Férias Letivas"`.
- No event of the categories `FINANCIAL`, `SECRETARIAT`, `UNIQUE_EVENTS` or `HOLIDAYS` was
  returned that day, so their shape is **unverified** beyond the common fields.
- `isCanceled` was always `false`. Skip canceled events.
- The calendar links to subjects only through `title` = subject name. Compare normalized names.

## Invoices

`graphql/Invoices.graphql`, variable `courseEnrollmentId` (declared `String!` here, `ID!` in the
other queries, exactly as the site does). A trimmed copy of the site's `GetInvoiceHomeModel`,
which the Financeiro page sends.

```
getInvoiceHomeModel: {
  informationType: string,   seen: PAYMENTS_ON_TIME
  billingDay: string,        seen: "10" (the day of the month invoices fall due)
  invoiceSections: [ {
    title: string,
    invoices: [ {
      id, status, title, shortTitle, formattedAmount, currentDueDate,
      block { type reason },
      details { paymentAlert { type message } invoiceAlert { type message } },
      hasExternalPayment
    } ]
  } ]
}
```

- On 2026-11-18 everything was paid: `informationType: PAYMENTS_ON_TIME`, `billingDay: "10"`,
  `invoiceSections: []`. **verified**
- **No open invoice has ever been observed.** The field names come from the query the site
  sends, but the format of `currentDueDate`, the values of `status`, `block.type`, `alert.type`
  and how `formattedAmount` looks are **unverified**.
- The site's code declares these enums, which are the best guess for the values:
  - invoice status: `OPEN`, `PAID`, `CANCELLED`, `BLOCKED`, `OVERDUE`, `PENDING`, `FAILED`,
    `UNDERPAID`, `OVERPAID`, `REFUNDED`, `WROTE_OFF`, `AVAILABLE`. **from the site's code**
  - `informationType`: `DOCUMENT_BLOCK`, `GENERIC_BLOCK`, `RE_ENROLLMENT_NOT_PAID`,
    `PAYMENTS_ON_TIME`, `NEGOTIATION`, `RE_ENROLLMENT_NEGOTIATION`, `NONE`. **from the site's code**
  - block type: `GENERIC`, `DOCUMENTS`, `RE_ENROLLMENT_NEGOTIATION_BLOCK`,
    `RE_ENROLLMENT_PENDING_DOCUMENT_BLOCK`. **from the site's code**
  - The site's client derives `OVERDUE` itself for instalments: `status == "OPEN" &&
    overdueDays > 0` becomes `OVERDUE`. So an overdue invoice may still arrive as `OPEN` with a
    past due date. **from the site's code**
  - The site treats `OPEN`, `OVERDUE` and `FAILED` as "to pay". **from the site's code**

## Boleto PDF

`graphql/BoletoPdf.graphql`: `getInvoiceBoletoPdf(courseEnrollmentId: String!, invoiceId:
String!) { url }`. **from the site's code**: never called, since no invoice was open. Whether
the URL expires, and whether it works for every invoice status, is **unverified**.

Other financial operations exist in the site's code and are not used:
`GetExternalPayment(...) { externalPaymentUrl }`, `GetNewPaymentStatement(courseEnrollmentId: ID!)`.
**from the site's code**

## Links the notifications can use

- Home: `https://www.anhanguera.com/aluno/home` (redirects to the course home when logged in).
- Course pages: `https://www.anhanguera.com/aluno/ava/<type>/<courseEnrollmentId>/...`, where
  `<type>` was `tecnologo` for the owner's course. The source of `<type>` in the API was not
  identified, so v1 links to the home page only.
