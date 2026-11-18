use std::future::Future;

use chrono::{DateTime, NaiveDate, Utc};
use chrono_tz::Tz;
use serde_json::{Value, json};

use crate::ava::model::{
    CalendarEvent, CourseEnrollment, InvoiceHome, Problem, Quiz, Subject, decode_boleto_url,
    decode_calendar, decode_course_enrollments, decode_invoices, decode_quizzes, decode_student_id,
    decode_subjects,
};
use crate::ava::{AvaClient, AvaError, Operation};

const CALENDAR_CATEGORIES: [&str; 5] = [
    "ACADEMIC_DATES",
    "UNIQUE_EVENTS",
    "SECRETARIAT",
    "FINANCIAL",
    "HOLIDAYS",
];

pub trait Portal: Send + Sync + 'static {
    fn student_id(&self) -> impl Future<Output = Result<String, AvaError>> + Send;

    fn course_enrollments(
        &self,
        student_id: &str,
    ) -> impl Future<Output = Result<Vec<CourseEnrollment>, AvaError>> + Send;

    fn invoices(
        &self,
        course_enrollment_id: &str,
    ) -> impl Future<Output = Result<InvoiceHome, AvaError>> + Send;

    fn boleto_url(
        &self,
        course_enrollment_id: &str,
        invoice_id: &str,
    ) -> impl Future<Output = Result<String, AvaError>> + Send;

    fn subjects(
        &self,
        course_enrollment_id: &str,
    ) -> impl Future<Output = Result<Vec<Subject>, AvaError>> + Send;

    fn quizzes(
        &self,
        subject_enrollment_id: &str,
    ) -> impl Future<Output = Result<Vec<Quiz>, AvaError>> + Send;

    fn calendar(
        &self,
        course_enrollment_id: &str,
        today: NaiveDate,
    ) -> impl Future<Output = Result<Vec<CalendarEvent>, AvaError>> + Send;

    fn login_backoff_until(&self) -> Option<DateTime<Utc>>;
}

impl AvaClient {
    async fn fetch<T>(
        &self,
        operation: Operation,
        variables: Value,
        decode: fn(&Value, Tz) -> Result<T, Problem>,
    ) -> Result<T, AvaError> {
        let body = self.execute(operation, variables).await?;
        decode(&body, self.tz()).map_err(AvaError::Schema)
    }
}

impl Portal for AvaClient {
    async fn student_id(&self) -> Result<String, AvaError> {
        self.fetch(Operation::StudentId, Value::Null, decode_student_id)
            .await
    }

    async fn course_enrollments(
        &self,
        student_id: &str,
    ) -> Result<Vec<CourseEnrollment>, AvaError> {
        let variables = json!({"studentId": student_id});
        self.fetch(
            Operation::CourseEnrollments,
            variables,
            decode_course_enrollments,
        )
        .await
    }

    async fn invoices(&self, course_enrollment_id: &str) -> Result<InvoiceHome, AvaError> {
        let variables = json!({"courseEnrollmentId": course_enrollment_id});
        self.fetch(Operation::Invoices, variables, decode_invoices)
            .await
    }

    async fn boleto_url(
        &self,
        course_enrollment_id: &str,
        invoice_id: &str,
    ) -> Result<String, AvaError> {
        let variables = json!({
            "courseEnrollmentId": course_enrollment_id,
            "invoiceId": invoice_id,
        });
        self.fetch(Operation::BoletoPdf, variables, decode_boleto_url)
            .await
    }

    async fn subjects(&self, course_enrollment_id: &str) -> Result<Vec<Subject>, AvaError> {
        let variables = json!({"courseEnrollmentId": course_enrollment_id});
        self.fetch(Operation::SubjectDeadlines, variables, decode_subjects)
            .await
    }

    async fn quizzes(&self, subject_enrollment_id: &str) -> Result<Vec<Quiz>, AvaError> {
        let variables = json!({"subjectEnrollmentId": subject_enrollment_id});
        self.fetch(Operation::UnitAssignments, variables, decode_quizzes)
            .await
    }

    async fn calendar(
        &self,
        course_enrollment_id: &str,
        today: NaiveDate,
    ) -> Result<Vec<CalendarEvent>, AvaError> {
        let variables = json!({
            "courseEnrollmentId": course_enrollment_id,
            "date": today.format("%Y-%m-%d").to_string(),
            "eventCategories": CALENDAR_CATEGORIES,
        });
        self.fetch(Operation::Calendar, variables, decode_calendar)
            .await
    }

    fn login_backoff_until(&self) -> Option<DateTime<Utc>> {
        AvaClient::login_backoff_until(self)
    }
}
