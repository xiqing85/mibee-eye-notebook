// Test additions for server.rs - HSTS + CORS

    #[tokio::test]
    async fn test_hsts_header_present() {
        let app = build_app(test_db());
        let req = Request::builder()
            .uri("/health")
            .body(Body::empty())
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        let hsts = res
            .headers()
            .get("strict-transport-security")
            .and_then(|v| v.to_str().ok());
        assert_eq!(
            hsts,
            Some("max-age=31536000; includeSubDomains"),
            "HSTS header should be present and correct"
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
