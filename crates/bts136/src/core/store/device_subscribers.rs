use super::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DeviceSubscriber {
    pub esn: u32,
    pub number: String,
    pub identity: Option<MobileIdentity>,
}

fn identity(kind: i64, value: u64) -> Result<MobileIdentity, StoreError> {
    match kind {
        0 => Ok(MobileIdentity::Tmsi20(
            u32::try_from(value).map_err(|_| StoreError::CorruptState)?,
        )),
        1 => Ok(MobileIdentity::Tmsi24(
            u32::try_from(value).map_err(|_| StoreError::CorruptState)?,
        )),
        2 => Ok(MobileIdentity::Min34(value)),
        3 => Ok(MobileIdentity::Imsi50(value)),
        _ => Err(StoreError::CorruptState),
    }
}

impl StateStore {
    pub(crate) fn device_subscribers(&self) -> Result<Vec<DeviceSubscriber>, StoreError> {
        let mut statement = self.connection.prepare("SELECT esn, number, identity_type, identity_value FROM esn_subscribers WHERE enabled=1 ORDER BY number")?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, u32>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<i64>>(2)?,
                row.get::<_, Option<i64>>(3)?,
            ))
        })?;
        rows.map(|row| {
            let (esn, number, kind, value) = row?;
            let identity = match (kind, value) {
                (Some(kind), Some(value)) => Some(identity(
                    kind,
                    u64::try_from(value).map_err(|_| StoreError::CorruptState)?,
                )?),
                (None, None) => None,
                _ => return Err(StoreError::CorruptState),
            };
            Ok(DeviceSubscriber {
                esn,
                number,
                identity,
            })
        })
        .collect()
    }

    pub(crate) fn save_device_subscriber(
        &mut self,
        device: &DeviceSubscriber,
    ) -> Result<(), StoreError> {
        let transaction = self.connection.transaction()?;
        if let Some(identity) = device.identity {
            let (kind, _, value) = identity.parts();
            transaction.execute("INSERT INTO subscribers(identity_type,identity_value,number,enabled) VALUES(?1,?2,?3,1) ON CONFLICT(identity_type,identity_value) DO UPDATE SET number=excluded.number,enabled=1", params![kind, value as i64, device.number])?;
        }
        let parts = device.identity.map(MobileIdentity::parts);
        transaction.execute("INSERT INTO esn_subscribers(esn,number,identity_type,identity_value,enabled) VALUES(?1,?2,?3,?4,1) ON CONFLICT(esn) DO UPDATE SET number=excluded.number,identity_type=excluded.identity_type,identity_value=excluded.identity_value,enabled=1", params![device.esn, device.number, parts.map(|p| p.0), parts.map(|p| p.2 as i64)])?;
        transaction.commit()?;
        Ok(())
    }

    pub(crate) fn disable_device_subscriber(
        &mut self,
        device: &DeviceSubscriber,
    ) -> Result<(), StoreError> {
        let transaction = self.connection.transaction()?;
        transaction.execute(
            "UPDATE esn_subscribers SET enabled=0 WHERE esn=?1",
            [device.esn],
        )?;
        if let Some(identity) = device.identity {
            let (kind, _, value) = identity.parts();
            transaction.execute(
                "UPDATE subscribers SET enabled=0 WHERE identity_type=?1 AND identity_value=?2",
                params![kind, value as i64],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn version_eleven_upgrade_preserves_legacy_subscribers_and_persists_devices() {
        let path = std::env::temp_dir().join(format!(
            "136bts-esn-migration-{}-{}.sqlite3",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut store = StateStore::open(&path).unwrap();
        let legacy = ProvisionedSubscriber {
            identity: MobileIdentity::Min34(7),
            number: "1001".into(),
        };
        store.seed_once(std::slice::from_ref(&legacy)).unwrap();
        store
            .connection
            .execute_batch("DROP TABLE sms_hub_parts; DROP TABLE sms_hub_outbox; DROP TABLE sms_hub_inbox; DROP TABLE esn_subscribers; PRAGMA user_version=11;")
            .unwrap();
        drop(store);
        let mut store = StateStore::open(&path).unwrap();
        assert_eq!(store.enabled_subscribers().unwrap(), vec![legacy]);
        let device = DeviceSubscriber {
            esn: 0x5ec4dbb7,
            number: "1500".into(),
            identity: None,
        };
        store.save_device_subscriber(&device).unwrap();
        drop(store);
        let store = StateStore::open(&path).unwrap();
        assert_eq!(store.device_subscribers().unwrap(), vec![device]);
        assert_eq!(
            store
                .connection
                .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
                .unwrap(),
            SCHEMA_VERSION
        );
        drop(store);
        std::fs::remove_file(path).unwrap();
    }
}
