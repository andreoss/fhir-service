mod support;

macro_rules! group {
    ($name:ident, $group:path) => {
        #[tokio::test]
        async fn $name() {
            let Some((store, client, namespace)) = support::fresh(stringify!($name)).await else {
                return;
            };
            $group(&store).await;
            support::drop_namespace(&client, &namespace).await;
        }
    };
}

group!(lifecycle, fhir_store_contract::lifecycle);
group!(versioning, fhir_store_contract::versioning);
group!(removal, fhir_store_contract::removal);
group!(record, fhir_store_contract::record);
group!(restore, fhir_store_contract::restore);
group!(readiness, fhir_store_contract::readiness);

group!(selection, fhir_store_contract::search::selection);
group!(qualifiers, fhir_store_contract::search::qualifiers);
group!(ordering, fhir_store_contract::search::ordering);
group!(linking, fhir_store_contract::search::linking);
group!(composites, fhir_store_contract::search::composites);
group!(targeted_index, fhir_store_contract::search::targeted_index);
group!(exempted, fhir_store_contract::search::exempted);
group!(
    narrowed_everywhere,
    fhir_store_contract::search::narrowed_everywhere
);
group!(
    converted_quantities,
    fhir_store_contract::search::converted_quantities
);
group!(shared_ids, fhir_store_contract::shared_ids);
group!(erased_versions, fhir_store_contract::erased_versions);

group!(atomicity, fhir_store_contract::atomicity);
group!(scoped_search, fhir_store_contract::scoped_search);
