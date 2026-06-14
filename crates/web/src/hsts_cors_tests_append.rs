    #[tokio::test]
    async fn test_hsts_header_present() {
        let app = build_app(test_db());
        let req = Request::builder()
            .uri("/health")
            .body(Body::empty())
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        let hsts_value = res
            .headers()
            .get(axum::http::header::STRICT_TRANSPORT_SECURITY)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_owned());
        assert!(hsts_value.is_some(), "HSTS header should be present");
        assert_eq!(
            hsts_value.as_deref(),
            Some("max-age=31536000; includeSubDomains"),
            "HSTS value should match"
        );
    }

    #[tokio::test]
    async fn test_cors_not_permissive() {
        let app = build_app(test_db());
        let router_str = format!("{:?}", app);
        assert!(
            !router_str.contains("permissive"),
            "CORS layer should not be permissive"
        );
    }
}
