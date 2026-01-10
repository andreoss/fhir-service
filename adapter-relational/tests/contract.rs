mod support;

macro_rules! group {
    ($name:ident, $group:path) => {
        #[tokio::test]
        async fn $name() {
            let Some((store, pool, namespace)) = support::fresh(stringify!($name)).await else {
                return;
            };
            $group(&store).await;
            support::drop_namespace(&pool, &namespace).await;
        }
    };
}

group!(lifecycle, fhir_store_contract::lifecycle);
group!(versioning, fhir_store_contract::versioning);
group!(removal, fhir_store_contract::removal);
group!(record, fhir_store_contract::record);
group!(readiness, fhir_store_contract::readiness);

group!(selection, fhir_store_contract::search::selection);
group!(qualifiers, fhir_store_contract::search::qualifiers);
group!(ordering, fhir_store_contract::search::ordering);
group!(linking, fhir_store_contract::search::linking);
group!(composites, fhir_store_contract::search::composites);
