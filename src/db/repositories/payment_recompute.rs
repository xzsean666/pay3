use std::collections::BTreeSet;

use bigdecimal::BigDecimal;
use sqlx::{Postgres, Row, Transaction};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::domain::OrderStatus;

use super::RepositoryError;

pub async fn recompute_orders_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    order_ids: BTreeSet<Uuid>,
) -> Result<(), RepositoryError> {
    if order_ids.is_empty() {
        return Ok(());
    }

    let order_ids = order_ids.into_iter().collect::<Vec<_>>();
    let rows = sqlx::query(
        r#"
        WITH locked_orders AS (
            SELECT id, expected_amount_raw, expires_at
            FROM orders
            WHERE id = ANY($1)
            ORDER BY id
            FOR UPDATE
        ),
        payment_totals AS (
            SELECT
                p.order_id,
                COALESCE(
                    SUM(p.amount_raw) FILTER (
                        WHERE p.match_status = 'on_time'
                          AND p.chain_status = 'confirmed'
                    ),
                    0
                ) AS confirmed_on_time_total,
                COALESCE(
                    SUM(p.amount_raw) FILTER (
                        WHERE p.match_status = 'on_time'
                          AND p.chain_status <> 'orphaned'
                    ),
                    0
                ) AS non_orphaned_on_time_total
            FROM payments p
            JOIN locked_orders o ON o.id = p.order_id
            GROUP BY p.order_id
        ),
        manual_acceptances AS (
            SELECT
                opo.order_id,
                opo.accepted_problem_payment_raw
            FROM order_payment_overrides opo
            JOIN locked_orders o ON o.id = opo.order_id
        )
        SELECT
            o.id,
            o.expected_amount_raw,
            o.expires_at,
            COALESCE(pt.confirmed_on_time_total, 0) AS confirmed_on_time_total,
            COALESCE(pt.non_orphaned_on_time_total, 0) AS non_orphaned_on_time_total,
            COALESCE(ma.accepted_problem_payment_raw, 0) AS accepted_problem_payment_raw
        FROM locked_orders o
        LEFT JOIN payment_totals pt ON pt.order_id = o.id
        LEFT JOIN manual_acceptances ma ON ma.order_id = o.id
        ORDER BY o.id
        "#,
    )
    .bind(&order_ids)
    .fetch_all(&mut **tx)
    .await
    .map_err(RepositoryError::Database)?;

    if rows.len() != order_ids.len() {
        return Err(RepositoryError::OrderNotFoundForRecompute { order_ids });
    }

    let now = OffsetDateTime::now_utc();
    for row in rows {
        let order_id: Uuid = row.try_get("id").map_err(RepositoryError::Database)?;
        let expected_amount: BigDecimal = row
            .try_get("expected_amount_raw")
            .map_err(RepositoryError::Database)?;
        let expires_at: OffsetDateTime = row
            .try_get("expires_at")
            .map_err(RepositoryError::Database)?;
        let confirmed_on_time_total: BigDecimal = row
            .try_get("confirmed_on_time_total")
            .map_err(RepositoryError::Database)?;
        let non_orphaned_on_time_total: BigDecimal = row
            .try_get("non_orphaned_on_time_total")
            .map_err(RepositoryError::Database)?;
        let accepted_problem_payment_raw: BigDecimal = row
            .try_get("accepted_problem_payment_raw")
            .map_err(RepositoryError::Database)?;
        let confirmed_accepted_total =
            confirmed_on_time_total.clone() + accepted_problem_payment_raw.clone();
        let non_orphaned_accepted_total = non_orphaned_on_time_total + accepted_problem_payment_raw;

        let status = recompute_status_from_totals(
            &expected_amount,
            &confirmed_accepted_total,
            &non_orphaned_accepted_total,
            now >= expires_at,
        );

        sqlx::query(
            r#"
            UPDATE orders
            SET paid_amount_raw = $2,
                status = $3,
                updated_at = now()
            WHERE id = $1
            "#,
        )
        .bind(order_id)
        .bind(confirmed_accepted_total)
        .bind(order_status_str(status))
        .execute(&mut **tx)
        .await
        .map_err(RepositoryError::Database)?;
    }

    Ok(())
}

fn recompute_status_from_totals(
    expected_amount: &BigDecimal,
    confirmed_on_time_total: &BigDecimal,
    non_orphaned_on_time_total: &BigDecimal,
    is_expired: bool,
) -> OrderStatus {
    if confirmed_on_time_total >= expected_amount {
        OrderStatus::Paid
    } else if non_orphaned_on_time_total >= expected_amount {
        OrderStatus::Confirming
    } else if non_orphaned_on_time_total > &BigDecimal::from(0) && !is_expired {
        OrderStatus::Partial
    } else if is_expired {
        OrderStatus::Expired
    } else {
        OrderStatus::Pending
    }
}

fn order_status_str(status: OrderStatus) -> &'static str {
    match status {
        OrderStatus::Pending => "pending",
        OrderStatus::Partial => "partial",
        OrderStatus::Confirming => "confirming",
        OrderStatus::Paid => "paid",
        OrderStatus::Expired => "expired",
    }
}
