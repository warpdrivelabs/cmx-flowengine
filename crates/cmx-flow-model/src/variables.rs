/*
 * @Describe: 流程变量容器。
 *
 * 变量是流程实例的可变状态载体：启动时传入、userTask 完成时合并、条件表达式读取、
 * serviceTask 读写。M1 用 JSON 作为统一值域（对齐 PG jsonb 列，天然可持久化），
 * 不引入独立的值类型系统——这与 cmx-core 的 DataValue 是不同关注点：DataValue 是
 * 「行列存储的强类型单元」，Variables 是「流程作用域的动态 KV」，故不复用。
 */

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

/// 流程实例变量集合。
///
/// 用 `BTreeMap` 而非 `HashMap`：键有序 → 序列化稳定 → 便于快照 diff 与测试断言。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Variables {
    map: BTreeMap<String, Value>,
}

impl Variables {
    /// 空变量集。
    pub fn new() -> Self {
        Self::default()
    }

    /// 从 JSON 对象构建；非对象则得到空集（宽容处理，不 panic）。
    pub fn from_json(value: Value) -> Self {
        match value {
            Value::Object(obj) => Self {
                map: obj.into_iter().collect(),
            },
            _ => Self::default(),
        }
    }

    /// 读取变量引用。
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.map.get(key)
    }

    /// 写入 / 覆盖变量。
    pub fn set(&mut self, key: impl Into<String>, value: Value) {
        self.map.insert(key.into(), value);
    }

    /// 合并另一组变量（同键覆盖）。userTask 完成时把提交的变量并入实例。
    pub fn merge(&mut self, other: Variables) {
        for (k, v) in other.map {
            self.map.insert(k, v);
        }
    }

    /// 是否为空。
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// 变量个数。
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// 导出为 `serde_json::Value::Object`，用于落 jsonb 列。
    pub fn to_json(&self) -> Value {
        Value::Object(self.map.clone().into_iter().collect())
    }

    /// 只读迭代。
    pub fn iter(&self) -> impl Iterator<Item = (&String, &Value)> {
        self.map.iter()
    }
}
