
#[derive(Clone, Debug)]
pub struct Case {
    
    pub shape: &'static str,
    
    pub name: String,
    
    pub method: &'static str,
    
    pub path: String,
    
    pub headers: Vec<(String, String)>,
    
    pub body: String,
}


pub struct Plan {
    cases: Vec<Case>,
}


#[derive(Clone, Debug)]
pub struct Answer {
    
    pub status: u16,
    
    pub headers: Vec<(String, String)>,
    
    pub body: String,
}

impl Answer {
    
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }
}

const SYSTEM: &str = "urn:shadow:identifier";

fn patient(id: &str, active: bool, family: &str) -> String {
    format!(
        "{{\"resourceType\":\"Patient\",\"id\":\"{id}\",\"active\":{active},\
         \"identifier\":[{{\"system\":\"{SYSTEM}\",\"value\":\"{id}\"}}],\
         \"name\":[{{\"family\":\"{family}\",\"given\":[\"Ada\"]}}],\
         \"gender\":\"female\",\"birthDate\":\"1974-03-02\"}}"
    )
}

fn observation(id: &str, subject: &str, value: f64) -> String {
    format!(
        "{{\"resourceType\":\"Observation\",\"id\":\"{id}\",\"status\":\"final\",\
         \"code\":{{\"coding\":[{{\"system\":\"http://loinc.org\",\"code\":\"29463-7\"}}]}},\
         \"subject\":{{\"reference\":\"Patient/{subject}\"}},\
         \"valueQuantity\":{{\"value\":{value},\"unit\":\"kg\",\
         \"system\":\"http://unitsofmeasure.org\",\"code\":\"kg\"}}}}"
    )
}

fn case(shape: &'static str, name: &str, method: &'static str, path: &str) -> Case {
    Case {
        shape,
        name: name.to_owned(),
        method,
        path: path.to_owned(),
        headers: Vec::new(),
        body: String::new(),
    }
}

fn with_body(mut case: Case, body: String) -> Case {
    case.headers.push((
        "content-type".to_owned(),
        "application/fhir+json".to_owned(),
    ));
    case.body = body;
    case
}

fn with_header(mut case: Case, name: &str, value: &str) -> Case {
    case.headers.push((name.to_owned(), value.to_owned()));
    case
}

impl Plan {
    
    pub fn of(cases: Vec<Case>) -> Plan {
        Plan { cases }
    }

    
    pub fn cases(&self) -> &[Case] {
        &self.cases
    }

    
    pub fn shapes(&self) -> Vec<&'static str> {
        let mut shapes: Vec<&'static str> = self.cases.iter().map(|case| case.shape).collect();
        shapes.sort_unstable();
        shapes.dedup();
        shapes
    }

    
    pub fn agreed() -> Plan {
        let mut cases = Vec::new();
        cases.push(with_body(
            case("create", "put-p1", "PUT", "/Patient/shdw-p1"),
            patient("shdw-p1", true, "Lovelace"),
        ));
        cases.push(with_body(
            case("create", "put-p2", "PUT", "/Patient/shdw-p2"),
            patient("shdw-p2", true, "Byron"),
        ));
        cases.push(with_body(
            case("create", "put-p3", "PUT", "/Patient/shdw-p3"),
            patient("shdw-p3", false, "Babbage"),
        ));
        cases.push(with_body(
            case("create", "put-o1", "PUT", "/Observation/shdw-o1"),
            observation("shdw-o1", "shdw-p1", 62.5),
        ));
        cases.push(with_body(
            case(
                "create-rejected",
                "put-mismatched",
                "PUT",
                "/Patient/shdw-p9",
            ),
            patient("shdw-other", true, "Mismatch"),
        ));
        cases.push(case("read", "read-p1", "GET", "/Patient/shdw-p1"));
        cases.push(case(
            "vread",
            "vread-p1-v1",
            "GET",
            "/Patient/shdw-p1/_history/1",
        ));
        cases.push(case(
            "read-missing",
            "read-absent",
            "GET",
            "/Patient/shdw-absent",
        ));
        cases.push(with_body(
            with_header(
                case("update", "update-p1", "PUT", "/Patient/shdw-p1"),
                "if-match",
                "{etag:read-p1}",
            ),
            patient("shdw-p1", false, "Lovelace"),
        ));
        cases.push(case("read", "read-p1-again", "GET", "/Patient/shdw-p1"));
        cases.push(with_body(
            with_header(
                case("update-stale", "update-p1-stale", "PUT", "/Patient/shdw-p1"),
                "if-match",
                "W/\"1\"",
            ),
            patient("shdw-p1", true, "Lovelace"),
        ));
        cases.push(with_body(
            with_header(
                case(
                    "conditional-create",
                    "conditional-create-p2",
                    "POST",
                    "/Patient",
                ),
                "if-none-exist",
                &format!("identifier={SYSTEM}|shdw-p2"),
            ),
            patient("shdw-p2-dup", true, "Byron"),
        ));
        cases.push(with_body(
            case(
                "conditional-update",
                "conditional-update-p2",
                "PUT",
                &format!("/Patient?identifier={SYSTEM}|shdw-p2"),
            ),
            patient("shdw-p2", false, "Byron"),
        ));
        cases.push(case("delete", "delete-p3", "DELETE", "/Patient/shdw-p3"));
        cases.push(case("read-deleted", "read-p3", "GET", "/Patient/shdw-p3"));
        cases.push(case(
            "delete-missing",
            "delete-absent",
            "DELETE",
            "/Patient/shdw-absent",
        ));
        cases.push(case(
            "history",
            "history-p1",
            "GET",
            "/Patient/shdw-p1/_history",
        ));
        cases.push(case(
            "history",
            "history-type",
            "GET",
            "/Patient/_history?_count=5",
        ));
        cases.push(case(
            "search",
            "search-by-id",
            "GET",
            "/Patient?_id=shdw-p1",
        ));
        cases.push(case(
            "search",
            "search-by-name",
            "GET",
            "/Patient?name=Lovelace",
        ));
        cases.push(case(
            "search",
            "search-by-reference",
            "GET",
            "/Observation?subject=Patient/shdw-p1",
        ));
        cases.push(case(
            "search-sorted",
            "search-sorted-by-family",
            "GET",
            "/Patient?_sort=family",
        ));
        cases.push(case(
            "search-sorted",
            "search-sorted-by-instant",
            "GET",
            "/Patient?_sort=-_lastUpdated",
        ));
        cases.push(case(
            "search-paging",
            "search-first-page",
            "GET",
            "/Patient?_count=1&_sort=_id",
        ));
        cases.push(case(
            "search-paging",
            "search-total-accurate",
            "GET",
            "/Patient?_count=1&_total=accurate&_sort=_id",
        ));
        cases.push(case(
            "search-unknown-parameter",
            "search-unknown",
            "GET",
            "/Patient?nosuchparameter=1",
        ));
        cases.push(with_body(
            case("bundle-transaction", "transaction-writes", "POST", "/"),
            transaction(),
        ));
        cases.push(with_body(
            case(
                "bundle-transaction-rolled-back",
                "transaction-rolled-back",
                "POST",
                "/",
            ),
            rolled_back(),
        ));
        cases.push(with_body(
            case("bundle-batch", "batch-mixed", "POST", "/"),
            batch(),
        ));
        cases.push(case(
            "read",
            "read-transaction-write",
            "GET",
            "/Patient/shdw-t1",
        ));
        cases.push(case(
            "read-missing",
            "read-rolled-back",
            "GET",
            "/Patient/shdw-r1",
        ));
        cases.push(case("capability", "capability", "GET", "/metadata"));
        Plan::of(cases)
    }
}

fn transaction() -> String {
    format!(
        "{{\"resourceType\":\"Bundle\",\"type\":\"transaction\",\"entry\":[\
         {{\"fullUrl\":\"urn:uuid:11111111-1111-4111-8111-111111111111\",\
         \"resource\":{},\"request\":{{\"method\":\"PUT\",\"url\":\"Patient/shdw-t1\"}}}},\
         {{\"fullUrl\":\"urn:uuid:22222222-2222-4222-8222-222222222222\",\
         \"resource\":{},\"request\":{{\"method\":\"PUT\",\"url\":\"Observation/shdw-t2\"}}}}]}}",
        patient("shdw-t1", true, "Hopper"),
        observation("shdw-t2", "shdw-t1", 70.0)
    )
}

fn rolled_back() -> String {
    format!(
        "{{\"resourceType\":\"Bundle\",\"type\":\"transaction\",\"entry\":[\
         {{\"resource\":{},\"request\":{{\"method\":\"PUT\",\"url\":\"Patient/shdw-r1\"}}}},\
         {{\"resource\":{{\"resourceType\":\"Patient\",\"id\":\"shdw-r2\"}},\
         \"request\":{{\"method\":\"PUT\",\"url\":\"Patient/shdw-mismatched\"}}}}]}}",
        patient("shdw-r1", true, "Rolled")
    )
}

fn batch() -> String {
    format!(
        "{{\"resourceType\":\"Bundle\",\"type\":\"batch\",\"entry\":[\
         {{\"resource\":{},\"request\":{{\"method\":\"PUT\",\"url\":\"Patient/shdw-b1\"}}}},\
         {{\"request\":{{\"method\":\"GET\",\"url\":\"Patient/shdw-absent\"}}}},\
         {{\"request\":{{\"method\":\"GET\",\"url\":\"Patient/shdw-p1\"}}}}]}}",
        patient("shdw-b1", true, "Batch")
    )
}
