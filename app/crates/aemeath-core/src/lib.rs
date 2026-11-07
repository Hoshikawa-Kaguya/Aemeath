//! 主程序的领域模型（术语见 GLOSSARY.md）：工作台上的节点与连线、槽位占用、连线规则，
//! 从工作台编译路由拓扑，以及配置文件的读写。
//!
//! - [`model`]：节点、连线、引擎设置等类型。
//! - [`workspace`]：工作台与全部编辑操作（保证槽位、连线等规则始终成立）。
//! - [`compile`]：编译成下发给引擎的路由拓扑。
//! - [`store`]：配置文件。
//!
//! 不含运行时状态（设备在线与否、槽位挂载与否），也不做 IO 以外的系统调用；这些属于 `aemeath-control`。

pub mod compile;
pub mod model;
pub mod store;
pub mod workspace;
