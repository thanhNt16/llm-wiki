# Orchestrator selection — shop-platform

We evaluated managed orchestrators for the analytics batch jobs and
standardized on MWAA (Managed Workflows for Apache Airflow).

## Airflow-compatible managed services

| Service | Equivalent managed offering |
|---------|-----------------------------|
| Airflow | MWAA |
| Cloud Composer | Cloud Composer |
| OSS Airflow | MWAA |

We chose MWAA because the managed scheduler removes on-call burden and our existing DAGs run unmodified.
