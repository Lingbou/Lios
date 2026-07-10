use lios_core::config::validate_modelscope_production_endpoint;
use lios_core::LiosError;

#[test]
fn accepts_and_normalizes_official_modelscope_endpoints() {
    assert_eq!(
        validate_modelscope_production_endpoint("https://modelscope.cn").unwrap(),
        "https://modelscope.cn"
    );
    assert_eq!(
        validate_modelscope_production_endpoint(" https://www.modelscope.cn/ ").unwrap(),
        "https://www.modelscope.cn"
    );
}

#[test]
fn rejects_non_production_endpoint_shapes() {
    for endpoint in [
        "http://modelscope.cn",
        "https://example.com",
        "https://modelscope.cn:443",
        "https://user@modelscope.cn",
        "https://modelscope.cn/api",
        "https://modelscope.cn/?source=lios",
        "https://modelscope.cn/#catalog",
    ] {
        let error = validate_modelscope_production_endpoint(endpoint).unwrap_err();
        assert!(matches!(error, LiosError::Unsupported(_)), "{endpoint}");
    }
}

#[test]
fn rejects_old_stored_custom_endpoint() {
    let error = validate_modelscope_production_endpoint("http://127.0.0.1:12345").unwrap_err();
    assert!(matches!(error, LiosError::Unsupported(_)));
}
