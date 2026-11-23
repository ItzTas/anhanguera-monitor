use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use anhanguera_monitor::ava::AvaError;
use anhanguera_monitor::ava::model::{
    CalendarEvent, CourseEnrollment, InformationType, InvoiceHome, Quiz, Subject,
};
use anhanguera_monitor::portal::Portal;
use chrono::{DateTime, NaiveDate, Utc};
use tokio::sync::Notify;

type Answer<T> = Result<T, AvaError>;

#[derive(Clone, Default)]
pub struct FakePortal {
    state: Arc<Mutex<State>>,
}

struct State {
    student_id: Answer<String>,
    enrollments: Answer<Vec<CourseEnrollment>>,
    invoices: HashMap<String, Answer<InvoiceHome>>,
    boleto: Answer<String>,
    subjects: HashMap<String, Answer<Vec<Subject>>>,
    quizzes: HashMap<String, Answer<Vec<Quiz>>>,
    calendar: HashMap<String, Answer<Vec<CalendarEvent>>>,
    backoff_until: Option<DateTime<Utc>>,
    gate: Option<Arc<Notify>>,
    calls: Vec<String>,
}

impl Default for State {
    fn default() -> State {
        State {
            student_id: Ok("student-1".to_owned()),
            enrollments: Ok(Vec::new()),
            invoices: HashMap::new(),
            boleto: Ok("https://boleto.example.test/slip.pdf".to_owned()),
            subjects: HashMap::new(),
            quizzes: HashMap::new(),
            calendar: HashMap::new(),
            backoff_until: None,
            gate: None,
            calls: Vec::new(),
        }
    }
}

impl FakePortal {
    pub fn new() -> FakePortal {
        FakePortal::default()
    }

    pub fn set_student_id(&self, answer: Answer<String>) {
        self.state().student_id = answer;
    }

    pub fn set_enrollments(&self, answer: Answer<Vec<CourseEnrollment>>) {
        self.state().enrollments = answer;
    }

    pub fn set_invoices(&self, course_enrollment_id: &str, answer: Answer<InvoiceHome>) {
        self.state()
            .invoices
            .insert(course_enrollment_id.to_owned(), answer);
    }

    pub fn set_boleto(&self, answer: Answer<String>) {
        self.state().boleto = answer;
    }

    pub fn set_subjects(&self, course_enrollment_id: &str, answer: Answer<Vec<Subject>>) {
        self.state()
            .subjects
            .insert(course_enrollment_id.to_owned(), answer);
    }

    pub fn set_quizzes(&self, subject_enrollment_id: &str, answer: Answer<Vec<Quiz>>) {
        self.state()
            .quizzes
            .insert(subject_enrollment_id.to_owned(), answer);
    }

    pub fn set_calendar(&self, course_enrollment_id: &str, answer: Answer<Vec<CalendarEvent>>) {
        self.state()
            .calendar
            .insert(course_enrollment_id.to_owned(), answer);
    }

    pub fn set_login_backoff_until(&self, until: Option<DateTime<Utc>>) {
        self.state().backoff_until = until;
    }

    // Holds the next student_id call, the first portal call of a cycle, until open_gate.
    pub fn gated(&self) {
        self.state().gate = Some(Arc::new(Notify::new()));
    }

    pub fn open_gate(&self) {
        if let Some(gate) = self.state().gate.take() {
            gate.notify_one();
        }
    }

    pub fn calls(&self) -> Vec<String> {
        self.state().calls.clone()
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn answer<T: Clone + Default>(
        &self,
        call: String,
        pick: impl Fn(&State) -> Option<&Answer<T>>,
    ) -> Answer<T> {
        let mut state = self.state();
        state.calls.push(call);
        pick(&state).map_or_else(|| Ok(T::default()), copied)
    }
}

fn copied<T: Clone>(answer: &Answer<T>) -> Answer<T> {
    match answer {
        Ok(value) => Ok(value.clone()),
        Err(AvaError::Unavailable(reason)) => Err(AvaError::Unavailable(reason.clone())),
        Err(AvaError::LoginRejected) => Err(AvaError::LoginRejected),
        Err(AvaError::Schema(problem)) => Err(AvaError::Schema(problem.clone())),
    }
}

pub fn quiet_home() -> InvoiceHome {
    InvoiceHome {
        information_type: InformationType::PaymentsOnTime,
        billing_day: None,
        invoices: Vec::new(),
    }
}

impl Portal for FakePortal {
    async fn student_id(&self) -> Result<String, AvaError> {
        let gate = self.state().gate.clone();
        if let Some(gate) = gate {
            gate.notified().await;
        }
        self.answer("student_id".to_owned(), |state| Some(&state.student_id))
    }

    async fn course_enrollments(
        &self,
        _student_id: &str,
    ) -> Result<Vec<CourseEnrollment>, AvaError> {
        self.answer("course_enrollments".to_owned(), |state| {
            Some(&state.enrollments)
        })
    }

    async fn invoices(&self, course_enrollment_id: &str) -> Result<InvoiceHome, AvaError> {
        let mut state = self.state();
        state.calls.push(format!("invoices:{course_enrollment_id}"));
        state
            .invoices
            .get(course_enrollment_id)
            .map_or_else(|| Ok(quiet_home()), copied)
    }

    async fn boleto_url(
        &self,
        course_enrollment_id: &str,
        invoice_id: &str,
    ) -> Result<String, AvaError> {
        self.answer(
            format!("boleto_url:{course_enrollment_id}:{invoice_id}"),
            |state| Some(&state.boleto),
        )
    }

    async fn subjects(&self, course_enrollment_id: &str) -> Result<Vec<Subject>, AvaError> {
        self.answer(format!("subjects:{course_enrollment_id}"), |state| {
            state.subjects.get(course_enrollment_id)
        })
    }

    async fn quizzes(&self, subject_enrollment_id: &str) -> Result<Vec<Quiz>, AvaError> {
        self.answer(format!("quizzes:{subject_enrollment_id}"), |state| {
            state.quizzes.get(subject_enrollment_id)
        })
    }

    async fn calendar(
        &self,
        course_enrollment_id: &str,
        today: NaiveDate,
    ) -> Result<Vec<CalendarEvent>, AvaError> {
        self.answer(
            format!("calendar:{course_enrollment_id}:{today}"),
            |state| state.calendar.get(course_enrollment_id),
        )
    }

    fn login_backoff_until(&self) -> Option<DateTime<Utc>> {
        self.state().backoff_until
    }
}
